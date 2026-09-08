use crate::error::SyncError;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use winit::event::Event;
use winit::event_loop::ControlFlow;

/// Status of the background sync engine.
///
/// Communicates the connectivity state of the source and destination directories
/// to the tray interface for visual tray signaling and tooltips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineStatus {
    /// Both source and destination directories are online and accessible.
    Healthy,
    /// Some (but not all) destination directories are offline.
    Degraded,
    /// The source directory is missing or unmounted.
    SourceOffline,
    /// The destination directory is missing or unmounted.
    DestinationOffline,
    /// Both directories are missing or unmounted.
    BothOffline,
}

/// Reason the tray event loop exited.
///
/// Returned by [`run_tray`] so the caller can decide whether to
/// re-launch the process after the tray icon has been cleanly dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayExitReason {
    /// User selected "Exit" from the tray menu.
    UserExit,
    /// User selected "Reload Config" — caller should re-spawn the process.
    Restart,
}

/// Initial state of a destination target for the system tray interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationState {
    pub path: PathBuf,
    pub is_online: bool,
}

/// Per-target status report sent from worker threads to the tray event loop.
#[derive(Debug, Clone)]
pub struct TargetStatusUpdate {
    pub target_index: usize,
    pub dest_online: bool,
}

/// Custom winit user event to wake up the loop on tray interactions and status updates.
///
/// This enum allows background worker threads and OS menu clicks to safely signal
/// the main thread UI event loop.
#[derive(Debug)]
pub enum UserEvent {
    /// A menu item click event forwarded from the tray menu callback.
    Menu(MenuEvent),
    /// A directory status change signal sent by the sync worker thread.
    StatusUpdate(TargetStatusUpdate),
    /// Watcher status update sent by the coordinator thread.
    WatcherStatus {
        source_online: bool,
        watcher_active: bool,
    },
    /// Result of an asynchronous configuration reload validation.
    ConfigReloadResult(Result<(), SyncError>),
}

/// Encapsulates visual and connectivity state tracking for the system tray interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    source_online: bool,
    watcher_active: bool,
    dest_online: Vec<bool>,
}

impl TrayState {
    /// Create a new TrayState with initial destination reachability states.
    pub fn new(initial_dest_online: Vec<bool>) -> Self {
        Self {
            source_online: false,
            watcher_active: false,
            dest_online: initial_dest_online,
        }
    }

    /// Access whether the source directory is currently online.
    pub fn source_online(&self) -> bool {
        self.source_online
    }

    /// Access whether the directory watcher is currently active.
    pub fn watcher_active(&self) -> bool {
        self.watcher_active
    }

    /// Access the per-destination online reachability slice.
    pub fn dest_online(&self) -> &[bool] {
        &self.dest_online
    }

    /// Update target destination reachability by index.
    pub fn update_target_status(&mut self, target_index: usize, online: bool) -> bool {
        if target_index < self.dest_online.len() {
            let changed = self.dest_online[target_index] != online;
            self.dest_online[target_index] = online;
            changed
        } else {
            false
        }
    }

    /// Update source directory connectivity and watcher active status.
    pub fn update_watcher_status(&mut self, source_online: bool, watcher_active: bool) -> bool {
        let changed = self.source_online != source_online || self.watcher_active != watcher_active;
        self.source_online = source_online;
        self.watcher_active = watcher_active;
        changed
    }

    /// Calculate the overall engine health status based on current state.
    pub fn overall_status(&self) -> EngineStatus {
        let all_dest_online =
            !self.dest_online.is_empty() && self.dest_online.iter().all(|&online| online);
        let any_dest_online = self.dest_online.iter().any(|&online| online);

        if !self.source_online || !self.watcher_active {
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
    pub fn online_dest_count(&self) -> usize {
        self.dest_online.iter().filter(|&&online| online).count()
    }

    /// Generate the formatted tooltip text for the system tray icon.
    pub fn tooltip_text(&self) -> String {
        let src_status_str = if !self.source_online {
            "Offline"
        } else if !self.watcher_active {
            "Degraded"
        } else {
            "Online"
        };
        format!(
            "syncdir — Src: {} | Dests: {}/{} Online",
            src_status_str,
            self.online_dest_count(),
            self.dest_online.len()
        )
    }
}

/// Generate a status-specific 32×32 RGBA tray icon.
fn generate_status_icon(status: EngineStatus) -> Result<Icon, SyncError> {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];

    // Color mappings based on status
    let (border_r, border_g, border_b) = match status {
        EngineStatus::Healthy => (66, 133, 244),           // Blue
        EngineStatus::Degraded => (255, 140, 0),           // Orange
        EngineStatus::SourceOffline => (219, 68, 85),      // Red
        EngineStatus::DestinationOffline => (244, 180, 0), // Yellow
        EngineStatus::BothOffline => (180, 180, 180),      // Gray
    };

    let (center_r, center_g, center_b) = match status {
        EngineStatus::Healthy => (255, 255, 255), // White
        _ => (80, 80, 80),                        // Dark gray
    };

    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let is_border = !(4..28).contains(&x) || !(4..28).contains(&y);
            if is_border {
                rgba[idx] = border_r;
                rgba[idx + 1] = border_g;
                rgba[idx + 2] = border_b;
                rgba[idx + 3] = 255;
            } else {
                rgba[idx] = center_r;
                rgba[idx + 1] = center_g;
                rgba[idx + 2] = center_b;
                rgba[idx + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size)
        .map_err(|e| SyncError::tray_with_source("Failed to create icon from RGBA buffer", e))
}

static ICON_CACHE: std::sync::OnceLock<std::collections::HashMap<EngineStatus, Icon>> =
    std::sync::OnceLock::new();

fn get_cached_icon(status: EngineStatus) -> Result<Icon, SyncError> {
    let cache = ICON_CACHE.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        for s in [
            EngineStatus::Healthy,
            EngineStatus::Degraded,
            EngineStatus::SourceOffline,
            EngineStatus::DestinationOffline,
            EngineStatus::BothOffline,
        ] {
            if let Ok(icon) = generate_status_icon(s) {
                m.insert(s, icon);
            }
        }
        m
    });
    cache
        .get(&status)
        .cloned()
        .ok_or_else(|| SyncError::tray(format!("No icon cached for {:?}", status)))
}

fn generate_default_icon() -> Result<Icon, SyncError> {
    get_cached_icon(EngineStatus::Healthy)
}

/// Display a native Windows About modal dialog box containing version, description, copyright, and URL.
#[cfg(target_os = "windows")]
fn show_about_dialog() {
    use std::os::windows::ffi::OsStrExt;
    let title: Vec<u16> = std::ffi::OsStr::new("About syncdir\0")
        .encode_wide()
        .collect();
    let msg_text = format!(
        "syncdir v{} — Windows background folder synchronization daemon\n{}\n{}\0",
        env!("CARGO_PKG_VERSION"),
        crate::COPYRIGHT,
        env!("CARGO_PKG_REPOSITORY")
    );
    let text: Vec<u16> = std::ffi::OsStr::new(&msg_text).encode_wide().collect();
    // SAFETY: MessageBoxW is a standard Win32 API function. Passing null hwnd and valid
    // null-terminated wide character array pointers is safe and opens a native modal dialog.
    unsafe {
        unsafe extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                utype: u32,
            ) -> i32;
        }
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            0x00000040,
        ); // MB_OK | MB_ICONINFORMATION
    }
}

#[cfg(not(target_os = "windows"))]
fn show_about_dialog() {}

/// Display a native Windows Error modal dialog box.
#[cfg(target_os = "windows")]
fn show_error_dialog(title_str: &str, msg_str: &str) {
    use std::os::windows::ffi::OsStrExt;
    let title_wide: Vec<u16> = std::ffi::OsStr::new(&format!("{}\0", title_str))
        .encode_wide()
        .collect();
    let msg_wide: Vec<u16> = std::ffi::OsStr::new(&format!("{}\0", msg_str))
        .encode_wide()
        .collect();
    // SAFETY: MessageBoxW is a standard Win32 API function.
    unsafe {
        unsafe extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                utype: u32,
            ) -> i32;
        }
        MessageBoxW(
            std::ptr::null_mut(),
            msg_wide.as_ptr(),
            title_wide.as_ptr(),
            0x00000010,
        ); // MB_OK | MB_ICONERROR
    }
}

#[cfg(not(target_os = "windows"))]
fn show_error_dialog(_title_str: &str, _msg_str: &str) {}

/// Handler interface for decoupled tray user actions.
///
/// Implementors handle UI actions triggered by context menu clicks, such as
/// initiating a manual sync, reloading the configuration, or toggling Windows startup.
pub trait TrayActionHandler: Send + Sync + 'static {
    /// Callback when user requests manual synchronization.
    fn on_sync_now(&self) -> Result<(), SyncError>;
    /// Callback when user requests configuration reload.
    fn on_reload_config(&self) -> Result<(), SyncError>;
    /// Callback when user toggles Windows startup registration.
    /// Returns the updated registration status.
    fn on_toggle_startup(&self, enable: bool) -> Result<bool, SyncError>;
    /// Query whether startup auto-run is currently registered.
    fn is_startup_enabled(&self) -> Result<bool, SyncError>;
    /// Open the active configuration file in system editor.
    fn on_open_config(&self) -> Result<(), SyncError> {
        Ok(())
    }
    /// Open the active logs directory in system file explorer.
    fn on_view_logs(&self) -> Result<(), SyncError> {
        Ok(())
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
pub fn run_tray<H: TrayActionHandler + ?Sized>(
    event_loop: winit::event_loop::EventLoop<UserEvent>,
    destinations: Vec<DestinationState>,
    handler: Arc<H>,
) -> Result<TrayExitReason, SyncError> {
    let dests: Vec<PathBuf> = destinations.iter().map(|d| d.path.clone()).collect();
    let initial_dest_online: Vec<bool> = destinations.iter().map(|d| d.is_online).collect();

    let open_config = MenuItem::new("Open Config", true, None);
    let reload_config = MenuItem::new("Reload Config", true, None);
    let view_logs = MenuItem::new("View Logs", true, None);
    let sync_now = MenuItem::new("Sync Now", true, None);

    let initially_checked = match handler.is_startup_enabled() {
        Ok(enabled) => enabled,
        Err(e) => {
            tracing::error!(error = %e, "Failed to query startup registration status");
            false
        }
    };
    let startup_toggle =
        CheckMenuItem::new("Start on System Startup", true, initially_checked, None);

    let about = MenuItem::new("About", true, None);
    let exit = MenuItem::new("Exit", true, None);

    let menu = Menu::new();
    menu.append(&open_config)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
    menu.append(&reload_config)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
    menu.append(&view_logs)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
    menu.append(&sync_now)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
    menu.append(&startup_toggle)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;

    // Add per-destination items
    let mut dest_menu_items = Vec::new();
    if !dests.is_empty() {
        let separator = PredefinedMenuItem::separator();
        menu.append(&separator)
            .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;

        for (i, d) in dests.iter().enumerate() {
            let is_online = initial_dest_online.get(i).copied().unwrap_or(false);
            let indicator = if is_online { "●" } else { "○" };
            let status_str = if is_online { "Online" } else { "Offline" };
            let label = format!("{} {} ({})", indicator, d.display(), status_str);
            let item = MenuItem::new(&label, false, None); // Read-only / disabled
            menu.append(&item)
                .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
            dest_menu_items.push(item);
        }
    }

    let separator_exit = PredefinedMenuItem::separator();
    menu.append(&separator_exit)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
    menu.append(&about)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;
    menu.append(&exit)
        .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;

    let icon = generate_default_icon()?;
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("syncdir — Folder Sync")
        .with_icon(icon)
        .build()
        .map_err(|e| SyncError::tray_with_source("Failed to create tray icon", e))?;

    // Set menu event handler to forward menu events to the event loop
    let proxy = event_loop.create_proxy();
    let reload_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let open_config_id = open_config.id().clone();
    let reload_config_id = reload_config.id().clone();
    let view_logs_id = view_logs.id().clone();
    let sync_now_id = sync_now.id().clone();
    let startup_toggle_id = startup_toggle.id().clone();
    let about_id = about.id().clone();
    let exit_id = exit.id().clone();

    let mut state = TrayState::new(initial_dest_online);
    let mut needs_repaint = true;
    let exit_reason = Rc::new(Cell::new(TrayExitReason::UserExit));
    let exit_reason_closure = exit_reason.clone();

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);

            match event {
                Event::UserEvent(UserEvent::Menu(menu_event)) => {
                    if menu_event.id == exit_id {
                        MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
                        elwt.exit();
                    } else if menu_event.id == sync_now_id {
                        if let Err(e) = handler.on_sync_now() {
                            tracing::error!(error = %e, "Manual sync failed");
                        } else {
                            tracing::info!("Manual sync triggered from tray menu");
                        }
                    } else if menu_event.id == open_config_id {
                        if let Err(e) = handler.on_open_config() {
                            let err_msg = format!("Failed to open config:\n\n{e}");
                            tracing::error!(error = %e, "Failed to open config file");
                            show_error_dialog("Open Config Error", &err_msg);
                        }
                    } else if menu_event.id == reload_config_id {
                        tracing::info!(
                            "Reload Config requested via tray menu; offloading to background thread."
                        );
                        let handler_clone = handler.clone();
                        let proxy_clone = reload_proxy.clone();
                        std::thread::spawn(move || {
                            let res = handler_clone.on_reload_config();
                            let _ = proxy_clone.send_event(UserEvent::ConfigReloadResult(res));
                        });
                    } else if menu_event.id == view_logs_id {
                        if let Err(e) = handler.on_view_logs() {
                            let err_msg = format!("Failed to open log directory:\n\n{e}");
                            tracing::error!(error = %e, "Failed to open log directory");
                            show_error_dialog("View Logs Error", &err_msg);
                        }
                    } else if menu_event.id == startup_toggle_id {
                        let is_checked = startup_toggle.is_checked();
                        match handler.on_toggle_startup(is_checked) {
                            Ok(actual_state) => {
                                startup_toggle.set_checked(actual_state);
                                tracing::info!(
                                    enabled = actual_state,
                                    "Startup auto-run toggled from tray menu"
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    error = %e,
                                    "Failed to toggle startup registration from tray"
                                );
                                startup_toggle.set_checked(!is_checked);
                            }
                        }
                    } else if menu_event.id == about_id {
                        show_about_dialog();
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloadResult(res)) => {
                    match res {
                        Ok(()) => {
                            tracing::info!(
                                "Configuration validated successfully. Restarting daemon..."
                            );
                            MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
                            exit_reason_closure.set(TrayExitReason::Restart);
                            elwt.exit();
                        }
                        Err(e) => {
                            let err_msg = format!("Configuration reload error:\n\n{e}");
                            tracing::error!(error = %e, "Configuration reload failed");
                            show_error_dialog("Config Reload Error", &err_msg);
                        }
                    }
                }
                Event::UserEvent(UserEvent::StatusUpdate(update)) => {
                    if state.update_target_status(update.target_index, update.dest_online) {
                        if update.target_index < dests.len() {
                            let d_path = &dests[update.target_index];
                            let status_str = if update.dest_online {
                                "Online"
                            } else {
                                "Offline"
                            };
                            let indicator = if update.dest_online { "●" } else { "○" };
                            dest_menu_items[update.target_index].set_text(format!(
                                "{} {} ({})",
                                indicator,
                                d_path.display(),
                                status_str
                            ));
                        }
                        needs_repaint = true;
                    }
                }
                Event::UserEvent(UserEvent::WatcherStatus {
                    source_online: so,
                    watcher_active: wa,
                }) => {
                    needs_repaint = state.update_watcher_status(so, wa) || needs_repaint;
                }
                _ => {}
            }

            if needs_repaint {
                needs_repaint = false;
                let status = state.overall_status();
                let new_tooltip = state.tooltip_text();

                let _ = tray_icon.set_tooltip(Some(&new_tooltip));
                if let Ok(new_icon) = get_cached_icon(status) {
                    let _ = tray_icon.set_icon(Some(new_icon));
                }
                tracing::info!(
                    status = ?status,
                    online_count = state.online_dest_count(),
                    "Tray status updated"
                );
            }
        })
        .map_err(|e| SyncError::tray_with_source("Event loop error", e))?;

    Ok(exit_reason.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_tray_state_initialization() {
        let state = TrayState::new(vec![true, false]);
        assert_eq!(state.source_online, false);
        assert_eq!(state.watcher_active, false);
        assert_eq!(state.dest_online, vec![true, false]);
        assert_eq!(state.online_dest_count(), 1);
    }

    #[test]
    fn test_tray_state_healthy() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(true, true);
        assert_eq!(state.overall_status(), EngineStatus::Healthy);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 2/2 Online"
        );
    }

    #[test]
    fn test_tray_state_source_offline() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(false, true);
        assert_eq!(state.overall_status(), EngineStatus::SourceOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Offline | Dests: 2/2 Online"
        );
    }

    #[test]
    fn test_tray_state_watcher_degraded() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(true, false);
        assert_eq!(state.overall_status(), EngineStatus::SourceOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Degraded | Dests: 2/2 Online"
        );
    }

    #[test]
    fn test_tray_state_degraded() {
        let mut state = TrayState::new(vec![true, true, false]);
        state.update_watcher_status(true, true);
        assert_eq!(state.overall_status(), EngineStatus::Degraded);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 2/3 Online"
        );
    }

    #[test]
    fn test_tray_state_destination_offline() {
        let mut state = TrayState::new(vec![false, false]);
        state.update_watcher_status(true, true);
        assert_eq!(state.overall_status(), EngineStatus::DestinationOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 0/2 Online"
        );
    }

    #[test]
    fn test_tray_state_both_offline() {
        let mut state = TrayState::new(vec![false, false]);
        state.update_watcher_status(false, true);
        assert_eq!(state.overall_status(), EngineStatus::BothOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Offline | Dests: 0/2 Online"
        );
    }

    #[test]
    fn test_tray_state_update_target_out_of_bounds() {
        let mut state = TrayState::new(vec![true]);
        assert!(!state.update_target_status(5, false));
        assert_eq!(state.dest_online(), &[true]);
    }

    #[test]
    fn test_tray_state_empty_destinations() {
        let mut state = TrayState::new(vec![]);
        state.update_watcher_status(true, true);
        assert_eq!(state.overall_status(), EngineStatus::Healthy);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 0/0 Online"
        );
    }
}

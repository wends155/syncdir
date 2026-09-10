pub(crate) mod assets;
pub(crate) use assets::{generate_default_icon, get_cached_icon};

use crate::error::SyncError;
use crate::sync::{ConnectivityState, SyncStatusObserver, WatcherState};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tray_icon::TrayIconBuilder;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use winit::event::Event;
use winit::event_loop::ControlFlow;

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

/// Reason the tray event loop exited.
///
/// Returned by [`TrayEventLoop::run`] so the caller can decide whether to
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
    path: PathBuf,
    is_online: ConnectivityState,
    resolved_unc: Option<PathBuf>,
    display_label: String,
}

impl DestinationState {
    /// Create a new DestinationState with path and online reachability.
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
}

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
    pub fn empty() -> Self {
        Self::new(Vec::<ConnectivityState>::new())
    }

    /// Access whether the source directory is currently online.
    pub fn source_online(&self) -> bool {
        self.source_online == ConnectivityState::Online
    }

    /// Access strongly-typed source connectivity state.
    pub fn source_connectivity(&self) -> ConnectivityState {
        self.source_online
    }

    /// Access whether the directory watcher is currently active.
    pub fn watcher_active(&self) -> bool {
        self.watcher_active == WatcherState::Active
    }

    /// Access strongly-typed directory watcher state.
    pub fn watcher_state(&self) -> WatcherState {
        self.watcher_active
    }

    /// Access the per-destination online reachability slice.
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
    pub fn scan_notice(&self) -> Option<&str> {
        self.scan_notice.as_deref()
    }

    /// Calculate the overall engine health status based on current state.
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
    pub fn online_dest_count(&self) -> usize {
        self.dest_online
            .iter()
            .filter(|&&online| online == ConnectivityState::Online)
            .count()
    }

    /// Generate the formatted tooltip text for the system tray icon.
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

/// Display a native Windows About modal dialog box containing version, description, copyright, and URL.
#[cfg(target_os = "windows")]
fn show_about_dialog() {
    let _ = std::thread::Builder::new()
        .name("about-dialog".to_string())
        .spawn(|| {
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
        });
}

#[cfg(not(target_os = "windows"))]
fn show_about_dialog() {}

/// Display a native Windows Error modal dialog box.
#[cfg(target_os = "windows")]
fn show_error_dialog(title_str: &str, msg_str: &str) {
    let title_owned = title_str.to_string();
    let msg_owned = msg_str.to_string();
    let _ = std::thread::Builder::new()
        .name("error-dialog".to_string())
        .spawn(move || {
            use std::os::windows::ffi::OsStrExt;
            let title_wide: Vec<u16> = std::ffi::OsStr::new(&format!("{}\0", title_owned))
                .encode_wide()
                .collect();
            let msg_wide: Vec<u16> = std::ffi::OsStr::new(&format!("{}\0", msg_owned))
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
        });
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
#[derive(Debug, Clone)]
pub(crate) struct TrayMenuIds {
    pub open_config_id: tray_icon::menu::MenuId,
    pub reload_config_id: tray_icon::menu::MenuId,
    pub view_logs_id: tray_icon::menu::MenuId,
    pub sync_now_id: tray_icon::menu::MenuId,
    pub startup_toggle_id: tray_icon::menu::MenuId,
    pub about_id: tray_icon::menu::MenuId,
    pub exit_id: tray_icon::menu::MenuId,
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
    reload_proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    is_reloading: Arc<std::sync::atomic::AtomicBool>,
    exit_reason: Rc<Cell<TrayExitReason>>,
    pub(crate) last_icon_status: Option<EngineStatus>,
}

impl<H: TrayActionHandler + ?Sized> TrayController<H> {
    /// Initialize the tray controller with menus, icons, and event proxies.
    pub fn new(
        event_loop: &winit::event_loop::EventLoop<UserEvent>,
        destinations: Vec<DestinationState>,
        handler: Arc<H>,
        exit_reason: Rc<Cell<TrayExitReason>>,
    ) -> Result<Self, SyncError> {
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

        let mut dest_menu_items = Vec::new();
        if !destinations.is_empty() {
            let separator = PredefinedMenuItem::separator();
            menu.append(&separator)
                .map_err(|e| SyncError::tray_with_source("Failed to append menu item", e))?;

            for d in &destinations {
                let is_online = d.is_online == ConnectivityState::Online;
                let indicator = if is_online { "●" } else { "○" };
                let status_str = if is_online { "Online" } else { "Offline" };
                let label = format!("{} {} ({})", indicator, d.display_label(), status_str);
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

        let proxy = event_loop.create_proxy();
        let reload_proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(UserEvent::Menu(event));
        }));

        let menu_ids = TrayMenuIds {
            open_config_id: open_config.id().clone(),
            reload_config_id: reload_config.id().clone(),
            view_logs_id: view_logs.id().clone(),
            sync_now_id: sync_now.id().clone(),
            startup_toggle_id: startup_toggle.id().clone(),
            about_id: about.id().clone(),
            exit_id: exit.id().clone(),
        };

        let initial_dest_online = destinations.iter().map(|d| d.is_online).collect::<Vec<_>>();
        let state = TrayState::new(initial_dest_online);
        let is_reloading = Arc::new(std::sync::atomic::AtomicBool::new(false));

        Ok(Self {
            tray_icon,
            menu_ids,
            startup_toggle,
            dest_menu_items,
            destinations,
            state,
            handler,
            reload_proxy,
            is_reloading,
            exit_reason,
            last_icon_status: None,
        })
    }

    /// Dispatch context menu selection events.
    pub fn handle_menu_event(
        &mut self,
        menu_event: MenuEvent,
        elwt: &winit::event_loop::EventLoopWindowTarget<UserEvent>,
    ) {
        if menu_event.id == self.menu_ids.exit_id {
            MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
            elwt.exit();
        } else if menu_event.id == self.menu_ids.sync_now_id {
            if let Err(e) = self.handler.on_sync_now() {
                tracing::error!(error = %e, "Manual sync failed");
            } else {
                tracing::info!("Manual sync triggered from tray menu");
            }
        } else if menu_event.id == self.menu_ids.open_config_id {
            if let Err(e) = self.handler.on_open_config() {
                let err_msg = format!("Failed to open config:\n\n{e}");
                tracing::error!(error = %e, "Failed to open config file");
                show_error_dialog("Open Config Error", &err_msg);
            }
        } else if menu_event.id == self.menu_ids.reload_config_id {
            if self
                .is_reloading
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
            {
                tracing::info!(
                    "Reload Config requested via tray menu; offloading to background thread."
                );
                let handler_clone = self.handler.clone();
                let proxy_clone = self.reload_proxy.clone();
                let reloading_flag = self.is_reloading.clone();
                match std::thread::Builder::new()
                    .name("config-reload".to_string())
                    .spawn(move || {
                        let res = handler_clone.on_reload_config();
                        reloading_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                        let _ = proxy_clone.send_event(UserEvent::ConfigReloadResult(res));
                    }) {
                    Ok(_) => {}
                    Err(e) => {
                        self.is_reloading
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                        tracing::error!(error = %e, "Failed to spawn config-reload background thread");
                        show_error_dialog(
                            "Reload Error",
                            &format!("Failed to spawn reload thread:\n\n{e}"),
                        );
                    }
                }
            } else {
                tracing::warn!(
                    "Configuration reload already in progress; ignoring duplicate request."
                );
            }
        } else if menu_event.id == self.menu_ids.view_logs_id {
            if let Err(e) = self.handler.on_view_logs() {
                let err_msg = format!("Failed to open log directory:\n\n{e}");
                tracing::error!(error = %e, "Failed to open log directory");
                show_error_dialog("View Logs Error", &err_msg);
            }
        } else if menu_event.id == self.menu_ids.startup_toggle_id {
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
                    tracing::error!(
                        error = %e,
                        "Failed to toggle startup registration from tray"
                    );
                    self.startup_toggle.set_checked(!is_checked);
                }
            }
        } else if menu_event.id == self.menu_ids.about_id {
            show_about_dialog();
        }
    }

    /// Handle the completion of an asynchronous configuration reload attempt.
    pub fn handle_config_reload_result(
        &mut self,
        res: Result<(), SyncError>,
        elwt: &winit::event_loop::EventLoopWindowTarget<UserEvent>,
    ) {
        match res {
            Ok(()) => {
                tracing::info!("Configuration validated successfully. Restarting daemon...");
                MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
                self.exit_reason.set(TrayExitReason::Restart);
                elwt.exit();
            }
            Err(e) => {
                let err_msg = format!("Configuration reload error:\n\n{e}");
                tracing::error!(error = %e, "Configuration reload failed");
                show_error_dialog("Config Reload Error", &err_msg);
            }
        }
    }

    /// Update per-destination connectivity and repaint tray icon and menu item text.
    pub fn handle_status_update(&mut self, update: TargetStatusUpdate) {
        if self
            .state
            .update_target_status(update.target_index, update.dest_online)
        {
            if update.target_index < self.destinations.len() {
                let d = &self.destinations[update.target_index];
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
                    tracing::warn!(
                        error = %e,
                        status = ?status,
                        "Failed to retrieve cached tray icon"
                    );
                }
            }
        }

        if icon_changed {
            tracing::info!(
                status = ?status,
                online_count = self.state.online_dest_count(),
                "Tray status updated"
            );
        } else {
            tracing::trace!(
                status = ?status,
                online_count = self.state.online_dest_count(),
                "Tray status unchanged"
            );
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
    event_loop: winit::event_loop::EventLoop<UserEvent>,
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
                Event::UserEvent(UserEvent::Menu(menu_event)) => {
                    controller.handle_menu_event(menu_event, elwt);
                }
                Event::UserEvent(UserEvent::ConfigReloadResult(res)) => {
                    controller.handle_config_reload_result(res, elwt);
                }
                Event::UserEvent(UserEvent::StatusUpdate(update)) => {
                    controller.handle_status_update(update);
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
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
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
    inner: winit::event_loop::EventLoop<UserEvent>,
}

impl TrayEventLoop {
    /// Create a new tray event loop.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Tray`] if the underlying windowing event loop fails to initialize.
    pub fn new() -> Result<Self, SyncError> {
        let inner = winit::event_loop::EventLoopBuilder::<UserEvent>::with_user_event()
            .build()
            .map_err(|e| SyncError::tray_with_source("Failed to create event loop", e))?;
        Ok(Self { inner })
    }

    /// Create an event proxy for dispatching events from background threads.
    pub(crate) fn create_proxy(&self) -> winit::event_loop::EventLoopProxy<UserEvent> {
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

#[cfg(target_os = "windows")]
fn system_root() -> PathBuf {
    std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
}

/// Launches the system file explorer targeting the specified path.
///
/// Returns `Err(std::io::Error)` with `ErrorKind::NotFound` if the path does not exist
/// or if explorer is not found.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Path does not exist: {}", path.display()),
        ));
    }
    #[cfg(target_os = "windows")]
    {
        let explorer = system_root().join("explorer.exe");
        if !explorer.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Explorer executable not found at {}", explorer.display()),
            ));
        }
        std::process::Command::new(explorer).arg(path).spawn()?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener).arg(path).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_tray_state_initialization() {
        let state = TrayState::new(vec![true, false]);
        assert_eq!(state.source_online(), false);
        assert_eq!(state.watcher_active(), false);
        assert_eq!(state.source_connectivity(), ConnectivityState::Offline);
        assert_eq!(state.watcher_state(), WatcherState::Inactive);
        assert_eq!(
            state.dest_online,
            vec![ConnectivityState::Online, ConnectivityState::Offline]
        );
        assert_eq!(state.online_dest_count(), 1);
    }

    #[test]
    fn test_tray_state_healthy() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert_eq!(state.overall_status(), EngineStatus::Healthy);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 2/2 Online"
        );
    }

    #[test]
    fn test_tray_state_source_offline() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(ConnectivityState::Offline, WatcherState::Active);
        assert_eq!(state.overall_status(), EngineStatus::SourceOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Offline | Dests: 2/2 Online"
        );
    }

    #[test]
    fn test_tray_state_watcher_degraded() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(ConnectivityState::Online, WatcherState::Inactive);
        assert_eq!(state.overall_status(), EngineStatus::SourceOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Degraded | Dests: 2/2 Online"
        );
    }

    #[test]
    fn test_tray_state_degraded() {
        let mut state = TrayState::new(vec![true, true, false]);
        state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert_eq!(state.overall_status(), EngineStatus::Degraded);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 2/3 Online"
        );
    }

    #[test]
    fn test_tray_state_destination_offline() {
        let mut state = TrayState::new(vec![false, false]);
        state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert_eq!(state.overall_status(), EngineStatus::DestinationOffline);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 0/2 Online"
        );
    }

    #[test]
    fn test_tray_state_both_offline() {
        let mut state = TrayState::new(vec![false, false]);
        state.update_watcher_status(ConnectivityState::Offline, WatcherState::Active);
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
        assert_eq!(state.dest_online(), &[ConnectivityState::Online]);
    }

    #[test]
    fn test_tray_state_empty_destinations() {
        let mut state = TrayState::empty();
        state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert_eq!(state.overall_status(), EngineStatus::Healthy);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 0/0 Online"
        );
    }

    #[test]
    fn test_tray_state_update_watcher_status_domain_enums() {
        use crate::sync::{ConnectivityState, WatcherState};
        let mut state = TrayState::new(vec![true]);
        let changed = state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert!(changed);
        let not_changed =
            state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert!(!not_changed);
    }

    #[test]
    fn test_tray_open_path_nonexistent() {
        let res = crate::tray::open_path(Path::new("Z:\\nonexistent_dir_12345\\missing"));
        assert!(res.is_err());
    }

    #[test]
    fn test_tray_open_path_nonexistent_returns_err() {
        let non_existent = std::path::Path::new("C:\\definitely_does_not_exist_tray_test_12345");
        let res = crate::tray::open_path(non_existent);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn test_tray_state_scan_notice() {
        let mut state = TrayState::new(vec![true, true]);
        state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 2/2 Online"
        );
        state.set_scan_notice(Some("Partial Scan (1 skipped)".to_string()));
        assert_eq!(
            state.tooltip_text(),
            "syncdir — Src: Online | Dests: 2/2 Online | Partial Scan (1 skipped)"
        );
        assert_eq!(state.scan_notice(), Some("Partial Scan (1 skipped)"));
    }

    #[test]
    fn test_destination_state_builder() {
        let dest = DestinationState::new("D:\\Sync", true)
            .with_resolved_unc(PathBuf::from("\\\\server\\share"));
        assert_eq!(dest.path, PathBuf::from("D:\\Sync"));
        assert_eq!(dest.is_online, ConnectivityState::Online);
        assert_eq!(dest.resolved_unc, Some(PathBuf::from("\\\\server\\share")));
    }

    #[test]
    fn test_engine_status_all_variants() {
        use pretty_assertions::assert_eq;
        use std::collections::HashSet;

        assert_eq!(EngineStatus::ALL.len(), 5);
        assert_eq!(EngineStatus::ALL[0], EngineStatus::Healthy);
        assert_eq!(EngineStatus::ALL[1], EngineStatus::Degraded);
        assert_eq!(EngineStatus::ALL[2], EngineStatus::SourceOffline);
        assert_eq!(EngineStatus::ALL[3], EngineStatus::DestinationOffline);
        assert_eq!(EngineStatus::ALL[4], EngineStatus::BothOffline);

        let unique: HashSet<EngineStatus> = EngineStatus::ALL.into_iter().collect();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn test_destination_state_display_label() {
        use pretty_assertions::assert_eq;
        use std::path::PathBuf;

        let dest_local = DestinationState::new("D:\\SyncFolder", true);
        assert_eq!(dest_local.display_label(), "D:\\SyncFolder");

        let dest_mapped = DestinationState::new("Z:\\Backup", true)
            .with_resolved_unc(PathBuf::from("\\\\192.168.1.50\\share\\backup"));
        assert_eq!(
            dest_mapped.display_label(),
            "Z:\\Backup -> \\\\192.168.1.50\\share\\backup"
        );
    }

    #[test]
    fn test_repaint_icon_gating_invariants() {
        // Verify that EngineStatus transition tracking behaves idempotently
        let mut last_icon_status: Option<EngineStatus> = None;
        let initial_status = EngineStatus::Healthy;

        // First repaint: transition detected
        let should_repaint_first = last_icon_status != Some(initial_status);
        assert!(should_repaint_first);
        last_icon_status = Some(initial_status);

        // Identical status: repaint elided
        let should_repaint_second = last_icon_status != Some(initial_status);
        assert!(!should_repaint_second);

        // State transition: repaint triggered
        let degraded_status = EngineStatus::Degraded;
        let should_repaint_transition = last_icon_status != Some(degraded_status);
        assert!(should_repaint_transition);
    }
}

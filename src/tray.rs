pub(crate) mod assets;

use crate::error::SyncError;
use crate::sync::{ConnectivityState, WatcherState};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use winit::event::Event;
use winit::event_loop::ControlFlow;

/// Open a file or directory in the system default application.
///
/// Strictly verifies that `%SystemRoot%\explorer.exe` exists as a file
/// before executing on Windows, preventing command hijack attacks.
pub(crate) fn open_path(path: &Path) -> Result<(), SyncError> {
    if !path.exists() {
        return Err(SyncError::validation(format!(
            "Path does not exist: {}",
            path.display()
        )));
    }
    #[cfg(target_os = "windows")]
    {
        let explorer = crate::config::system_root().join("explorer.exe");
        if !explorer.is_file() {
            return Err(SyncError::validation(format!(
                "Explorer executable not found at {}",
                explorer.display()
            )));
        }
        std::process::Command::new(explorer)
            .arg(path)
            .spawn()
            .map_err(SyncError::Io)?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(path)
            .spawn()
            .map_err(SyncError::Io)?;
    }
    Ok(())
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
    pub is_online: ConnectivityState,
    pub resolved_unc: Option<PathBuf>,
}

impl DestinationState {
    /// Create a new DestinationState with path and online reachability.
    pub fn new(path: impl Into<PathBuf>, is_online: impl Into<ConnectivityState>) -> Self {
        Self {
            path: path.into(),
            is_online: is_online.into(),
            resolved_unc: None,
        }
    }

    /// Builder method to attach a resolved alternate UNC path.
    pub fn with_resolved_unc(mut self, resolved_unc: impl Into<Option<PathBuf>>) -> Self {
        self.resolved_unc = resolved_unc.into();
        self
    }
}

/// Per-target status report sent from worker threads to the tray event loop.
#[derive(Debug, Clone)]
pub struct TargetStatusUpdate {
    pub target_index: usize,
    pub dest_online: ConnectivityState,
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

    /// Legacy boolean updater for backward compatibility.
    #[deprecated(
        since = "0.1.14",
        note = "use update_watcher_status with ConnectivityState and WatcherState"
    )]
    pub fn update_watcher_status_bool(
        &mut self,
        source_online: bool,
        watcher_active: bool,
    ) -> bool {
        self.update_watcher_status(source_online.into(), watcher_active.into())
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
pub struct TrayController<H: TrayActionHandler + ?Sized> {
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
                let path_label = match &d.resolved_unc {
                    Some(unc) => format!("{} -> {}", d.path.display(), unc.display()),
                    None => format!("{}", d.path.display()),
                };
                let label = format!("{} {} ({})", indicator, path_label, status_str);
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
                let _ = std::thread::Builder::new()
                    .name("config-reload".to_string())
                    .spawn(move || {
                        let res = handler_clone.on_reload_config();
                        reloading_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                        let _ = proxy_clone.send_event(UserEvent::ConfigReloadResult(res));
                    });
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
                let path_label = match &d.resolved_unc {
                    Some(unc) => format!("{} -> {}", d.path.display(), unc.display()),
                    None => format!("{}", d.path.display()),
                };
                self.dest_menu_items[update.target_index]
                    .set_text(format!("{} {} ({})", indicator, path_label, status_str));
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
        let _ = self.tray_icon.set_tooltip(Some(&new_tooltip));
        if let Ok(new_icon) = get_cached_icon(status) {
            let _ = self.tray_icon.set_icon(Some(new_icon));
        }
        tracing::info!(
            status = ?status,
            online_count = self.state.online_dest_count(),
            "Tray status updated"
        );
    }

    /// Retrieve the current exit reason.
    pub fn exit_reason(&self) -> TrayExitReason {
        self.exit_reason.get()
    }

    /// Read-only access to inner TrayState for inspection.
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
pub fn run_tray<H: TrayActionHandler + ?Sized>(
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
    #[allow(deprecated)]
    fn test_tray_state_update_watcher_status_bool_shim() {
        let mut state = TrayState::new(vec![true]);
        assert!(state.update_watcher_status_bool(true, true));
        assert_eq!(state.source_connectivity(), ConnectivityState::Online);
        assert_eq!(state.watcher_state(), WatcherState::Active);
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
        let res = open_path(Path::new("Z:\\nonexistent_dir_12345\\missing"));
        assert!(res.is_err());
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
}

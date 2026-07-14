use crate::error::SyncError;
use crate::startup::StartupRegistry;
use crate::sync::SyncCommand;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use winit::event::Event;
use winit::event_loop::ControlFlow;

/// Status of the background sync engine.
///
/// Communicates the connectivity state of the source and destination directories
/// to the tray interface for visual tray signaling and tooltips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineStatus {
    /// Both source and destination directories are online and accessible.
    Healthy,
    /// The source directory is missing or unmounted.
    SourceOffline,
    /// The destination directory is missing or unmounted.
    DestinationOffline,
    /// Both directories are missing or unmounted.
    BothOffline,
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
    Status(EngineStatus),
}

/// Generate a status-specific 32×32 RGBA tray icon.
fn generate_status_icon(status: EngineStatus) -> Result<Icon, SyncError> {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];

    // Color mappings based on status
    let (border_r, border_g, border_b) = match status {
        EngineStatus::Healthy => (66, 133, 244),           // Blue
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
    Icon::from_rgba(rgba, size, size).map_err(|e| SyncError::Tray(e.to_string()))
}

fn generate_default_icon() -> Result<Icon, SyncError> {
    generate_status_icon(EngineStatus::Healthy)
}

/// Open a file or directory in the system default application.
fn open_path(path: &std::path::Path) -> Result<(), SyncError> {
    std::process::Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map_err(SyncError::Io)?;
    Ok(())
}

/// Launch the system tray event loop (blocking).
///
/// Creates a tray icon in the Windows notification area with a checkable
/// context menu and listens for user mouse interactions and directory status updates.
///
/// # Arguments
///
/// * `event_loop` - The winit event loop initialized on the main UI thread.
/// * `config_path` - The system path to the user's `config.toml`.
/// * `log_dir` - The path to the active log directory for manual retrieval.
/// * `tx` - Sender channel used to dispatch sync commands to the worker.
///
/// # Returns
///
/// Returns `Ok(())` when the daemon is shut down via the "Exit" menu item.
///
/// # Errors
///
/// Returns [`SyncError::Tray`] if the tray menu, icon, or event loop builder fails.
pub fn run_tray(
    event_loop: winit::event_loop::EventLoop<UserEvent>,
    config_path: PathBuf,
    log_dir: PathBuf,
    tx: Sender<SyncCommand>,
) -> Result<(), SyncError> {
    let open_config = MenuItem::new("Open Config", true, None);
    let view_logs = MenuItem::new("View Logs", true, None);
    let sync_now = MenuItem::new("Sync Now", true, None);

    let initially_checked = StartupRegistry::is_registered().unwrap_or(false);
    let startup_toggle =
        CheckMenuItem::new("Start on System Startup", true, initially_checked, None);

    let exit = MenuItem::new("Exit", true, None);

    let menu = Menu::new();
    menu.append(&open_config)
        .map_err(|e| SyncError::Tray(e.to_string()))?;
    menu.append(&view_logs)
        .map_err(|e| SyncError::Tray(e.to_string()))?;
    menu.append(&sync_now)
        .map_err(|e| SyncError::Tray(e.to_string()))?;
    menu.append(&startup_toggle)
        .map_err(|e| SyncError::Tray(e.to_string()))?;
    menu.append(&exit)
        .map_err(|e| SyncError::Tray(e.to_string()))?;

    let icon = generate_default_icon()?;
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("syncdir — Folder Sync")
        .with_icon(icon)
        .build()
        .map_err(|e| SyncError::Tray(format!("Failed to create tray icon: {e}")))?;

    // Set menu event handler to forward menu events to the event loop
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let open_config_id = open_config.id().clone();
    let view_logs_id = view_logs.id().clone();
    let sync_now_id = sync_now.id().clone();
    let startup_toggle_id = startup_toggle.id().clone();
    let exit_id = exit.id().clone();

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);

            match event {
                Event::UserEvent(UserEvent::Menu(menu_event)) => {
                    if menu_event.id == exit_id {
                        MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
                        elwt.exit();
                    } else if menu_event.id == sync_now_id {
                        let _ = tx.send(SyncCommand::TriggerFullScan);
                        tracing::info!("Manual sync triggered from tray menu");
                    } else if menu_event.id == open_config_id {
                        let _ = open_path(&config_path);
                    } else if menu_event.id == view_logs_id {
                        let _ = open_path(&log_dir);
                    } else if menu_event.id == startup_toggle_id {
                        let is_checked = startup_toggle.is_checked();
                        if is_checked {
                            match StartupRegistry::register() {
                                Ok(()) => {
                                    tracing::info!("Startup auto-run registered via tray menu");
                                }
                                Err(e) => {
                                    tracing::error!("Failed to register startup from tray: {e}");
                                    startup_toggle.set_checked(false);
                                }
                            }
                        } else {
                            match StartupRegistry::unregister() {
                                Ok(()) => {
                                    tracing::info!("Startup auto-run unregistered via tray menu");
                                }
                                Err(e) => {
                                    tracing::error!("Failed to unregister startup from tray: {e}");
                                    startup_toggle.set_checked(true);
                                }
                            }
                        }
                    }
                }
                Event::UserEvent(UserEvent::Status(status)) => {
                    let new_tooltip = match status {
                        EngineStatus::Healthy => "syncdir — Folder Sync (Healthy)",
                        EngineStatus::SourceOffline => "syncdir — Warning: Source Offline",
                        EngineStatus::DestinationOffline => {
                            "syncdir — Warning: Destination Offline"
                        }
                        EngineStatus::BothOffline => "syncdir — Error: Both Folders Offline",
                    };
                    let _ = tray_icon.set_tooltip(Some(new_tooltip));
                    if let Ok(new_icon) = generate_status_icon(status) {
                        let _ = tray_icon.set_icon(Some(new_icon));
                    }
                    tracing::info!(status = ?status, "Tray status updated");
                }
                _ => {}
            }
        })
        .map_err(|e| SyncError::Tray(format!("Event loop error: {e}")))?;

    Ok(())
}

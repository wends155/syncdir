use crate::error::SyncError;
use crate::startup::StartupRegistry;
use crate::sync::SyncCommand;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use winit::event::Event;
use winit::event_loop::{ControlFlow, EventLoopBuilder};

/// Custom winit user event to wake up the loop on tray interactions.
#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
}

/// Generate a simple 32×32 RGBA tray icon (blue square with white center).
fn generate_default_icon() -> Result<Icon, SyncError> {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let is_border = !(4..28).contains(&x) || !(4..28).contains(&y);
            if is_border {
                // Blue border
                rgba[idx] = 66; // R
                rgba[idx + 1] = 133; // G
                rgba[idx + 2] = 244; // B
                rgba[idx + 3] = 255; // A
            } else {
                // White center
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).map_err(|e| SyncError::Tray(e.to_string()))
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
/// Creates a tray icon with a right-click context menu and runs the
/// winit event loop. Returns when the user selects "Exit".
///
/// # Errors
/// Returns `SyncError::Tray` if the tray icon or event loop cannot be created.
pub fn run_tray(
    config_path: PathBuf,
    log_dir: PathBuf,
    tx: Sender<SyncCommand>,
) -> Result<(), SyncError> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| SyncError::Tray(format!("Failed to create event loop: {e}")))?;

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
    let _tray_icon = TrayIconBuilder::new()
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

            if let Event::UserEvent(UserEvent::Menu(menu_event)) = event {
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
        })
        .map_err(|e| SyncError::Tray(format!("Event loop error: {e}")))?;

    Ok(())
}

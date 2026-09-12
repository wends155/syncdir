//! Context menu definitions, IDs, and action handling trait for the system tray.

use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};

use crate::error::SyncError;
use crate::sync::ConnectivityState;
use crate::tray::state::DestinationState;

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

/// Identifiers for context menu items to map click events to actions.
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

/// Structure containing the created context menu and its dynamic item handles.
pub(crate) struct TrayMenuComponents {
    pub menu: Menu,
    pub menu_ids: TrayMenuIds,
    pub startup_toggle: CheckMenuItem,
    pub dest_menu_items: Vec<MenuItem>,
}

/// Build the native tray context menu with destination indicators and control actions.
pub(crate) fn build_tray_menu<H: TrayActionHandler + ?Sized>(
    destinations: &[DestinationState],
    handler: &Arc<H>,
) -> Result<TrayMenuComponents, SyncError> {
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

        for d in destinations {
            let is_online = d.is_online() == ConnectivityState::Online;
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

    let menu_ids = TrayMenuIds {
        open_config_id: open_config.id().clone(),
        reload_config_id: reload_config.id().clone(),
        view_logs_id: view_logs.id().clone(),
        sync_now_id: sync_now.id().clone(),
        startup_toggle_id: startup_toggle.id().clone(),
        about_id: about.id().clone(),
        exit_id: exit.id().clone(),
    };

    Ok(TrayMenuComponents {
        menu,
        menu_ids,
        startup_toggle,
        dest_menu_items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHandler {
        startup: bool,
    }

    impl TrayActionHandler for DummyHandler {
        fn on_sync_now(&self) -> Result<(), SyncError> {
            Ok(())
        }
        fn on_reload_config(&self) -> Result<(), SyncError> {
            Ok(())
        }
        fn on_toggle_startup(&self, enable: bool) -> Result<bool, SyncError> {
            Ok(enable)
        }
        fn is_startup_enabled(&self) -> Result<bool, SyncError> {
            Ok(self.startup)
        }
    }

    #[test]
    fn test_tray_action_handler_defaults() {
        let handler = DummyHandler { startup: false };
        assert!(handler.on_open_config().is_ok());
        assert!(handler.on_view_logs().is_ok());
        assert!(!handler.is_startup_enabled().unwrap());
    }

    #[test]
    fn test_build_tray_menu_empty_destinations() {
        let handler = Arc::new(DummyHandler { startup: true });
        let res = build_tray_menu(&[], &handler);
        assert!(res.is_ok());
        let components = res.unwrap();
        assert!(components.dest_menu_items.is_empty());
        assert!(components.startup_toggle.is_checked());
    }

    #[test]
    fn test_build_tray_menu_with_destinations() {
        let handler = Arc::new(DummyHandler { startup: false });
        let destinations = vec![
            DestinationState::new(
                std::path::PathBuf::from(r"\\server\share"),
                ConnectivityState::Online,
            ),
            DestinationState::new(
                std::path::PathBuf::from(r"D:\backup"),
                ConnectivityState::Offline,
            ),
        ];
        let res = build_tray_menu(&destinations, &handler);
        assert!(res.is_ok());
        let components = res.unwrap();
        assert_eq!(components.dest_menu_items.len(), 2);
        assert!(!components.startup_toggle.is_checked());
    }
}

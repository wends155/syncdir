//! Startup Registry module for syncdir.
//!
//! This module provides target-specific methods to configure the application
//! to automatically launch at user login session via the Windows Registry.

use crate::error::SyncError;

#[cfg(windows)]
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
#[cfg(windows)]
use winreg::RegKey;

/// Manages the Windows Startup Run registry key for syncdir.
pub struct StartupRegistry;

#[cfg(windows)]
impl StartupRegistry {
    /// Registry value format shared by register and is_registered.
    fn registry_value() -> Result<String, SyncError> {
        let exe_path = std::env::current_exe().map_err(SyncError::Io)?;
        Ok(format!("\"{}\" --autostart", exe_path.to_string_lossy()))
    }

    /// Checks if the startup registration exists and matches the current exe.
    ///
    /// # Errors
    /// Returns `SyncError::Io` if retrieving the current executable path fails.
    pub fn is_registered() -> Result<bool, SyncError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let run_key =
            hkcu.open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_READ);
        match run_key {
            Ok(key) => {
                let val: String = match key.get_value("syncdir") {
                    Ok(v) => v,
                    Err(_) => return Ok(false),
                };
                Ok(val == Self::registry_value()?)
            }
            Err(_) => Ok(false),
        }
    }

    /// Registers the current exe path in HKCU run key with --autostart flag.
    ///
    /// # Errors
    /// Returns `SyncError::Config` if registry write operations fail.
    pub fn register() -> Result<(), SyncError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| SyncError::Config(format!("Failed to open Run registry key: {e}")))?;
        key.set_value("syncdir", &Self::registry_value()?)
            .map_err(|e| SyncError::Config(format!("Failed to write registry value: {e}")))?;
        Ok(())
    }

    /// Removes the syncdir value from HKCU run key.
    ///
    /// Silently succeeds if the value does not exist.
    pub fn unregister() -> Result<(), SyncError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) =
            hkcu.open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_WRITE)
        {
            let _ = key.delete_value("syncdir");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
impl StartupRegistry {
    /// Checks if the startup registration exists. Always returns false on non-Windows.
    pub fn is_registered() -> Result<bool, SyncError> {
        Ok(false)
    }
    /// Startup registration mock. No-op on non-Windows.
    pub fn register() -> Result<(), SyncError> {
        Ok(())
    }
    /// Startup unregistration mock. No-op on non-Windows.
    pub fn unregister() -> Result<(), SyncError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn test_startup_registration_toggle() {
        struct StateGuard(bool);
        impl Drop for StateGuard {
            fn drop(&mut self) {
                if self.0 {
                    let _ = StartupRegistry::register();
                } else {
                    let _ = StartupRegistry::unregister();
                }
            }
        }

        let initially_registered = StartupRegistry::is_registered().unwrap_or(false);
        let _guard = StateGuard(initially_registered);

        StartupRegistry::unregister().unwrap();
        assert!(!StartupRegistry::is_registered().unwrap());

        StartupRegistry::register().unwrap();
        assert!(StartupRegistry::is_registered().unwrap());

        StartupRegistry::unregister().unwrap();
        assert!(!StartupRegistry::is_registered().unwrap());
    }
}

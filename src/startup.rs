//! Startup Registry module for syncdir.
//!
//! This module provides target-specific methods to configure the application
//! to automatically launch at user login session via the Windows Registry.

use crate::error::SyncError;

#[cfg(windows)]
use winreg::RegKey;
#[cfg(windows)]
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

/// Manages the Windows Startup Run registry key for syncdir.
pub struct StartupRegistry;

#[cfg(windows)]
impl StartupRegistry {
    /// Registry value format shared by register and is_registered.
    fn registry_value() -> Result<String, SyncError> {
        static CACHED_VALUE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        if let Some(val) = CACHED_VALUE.get() {
            return Ok(val.clone());
        }
        let exe_path = std::env::current_exe().map_err(SyncError::Io)?;
        let val = format!("\"{}\" --autostart", exe_path.to_string_lossy());
        CACHED_VALUE.set(val.clone()).ok();
        Ok(val)
    }

    /// Checks whether the syncdir value exists in HKCU run key.
    ///
    /// # Errors
    /// * Returns `SyncError::Registry` if the registry key cannot be opened
    ///   or read for reasons other than `ErrorKind::NotFound` (e.g., access denied).
    /// * Returns `SyncError::Io` if retrieving the current executable path fails.
    pub fn is_registered() -> Result<bool, SyncError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu
            .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_READ)
        {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(SyncError::registry_with_source(
                    "Failed to open Run registry key",
                    e,
                ));
            }
        };
        match key.get_value::<String, _>("syncdir") {
            Ok(val) => Ok(val.eq_ignore_ascii_case(&Self::registry_value()?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SyncError::registry_with_source(
                "Failed to read syncdir registry value",
                e,
            )),
        }
    }

    /// Registers the current exe path in HKCU run key with --autostart flag.
    ///
    /// # Errors
    /// Returns `SyncError::Registry` if registry write operations fail.
    pub fn register() -> Result<(), SyncError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| SyncError::registry_with_source("Failed to open Run registry key", e))?;
        key.set_value("syncdir", &Self::registry_value()?)
            .map_err(|e| SyncError::registry_with_source("Failed to write registry value", e))?;
        Ok(())
    }

    /// Removes the syncdir value from HKCU run key.
    ///
    /// Silently succeeds if the value or key does not exist.
    ///
    /// # Errors
    /// Returns `SyncError::Registry` if the registry key cannot be opened
    /// or the value cannot be deleted for reasons other than `ErrorKind::NotFound`.
    pub fn unregister() -> Result<(), SyncError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu
            .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_WRITE)
        {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(SyncError::registry_with_source(
                    "Failed to open Run registry key for deletion",
                    e,
                ));
            }
        };
        match key.delete_value("syncdir") {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SyncError::registry_with_source(
                "Failed to delete syncdir registry value",
                e,
            )),
        }
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

/// Trait abstraction for Windows Startup Registry operations.
pub trait RegistryBackend {
    /// Checks if the startup registration exists.
    fn is_registered(&self) -> Result<bool, SyncError>;
    /// Registers the application in startup registry.
    fn register(&self) -> Result<(), SyncError>;
    /// Removes the application from startup registry.
    fn unregister(&self) -> Result<(), SyncError>;
}

/// In-memory mock startup registry for cross-platform unit testing.
#[derive(Debug, Default, Clone)]
pub struct MockStartupRegistry {
    registered: std::sync::Arc<std::sync::Mutex<bool>>,
}

impl MockStartupRegistry {
    /// Create a new mock registry with given initial registration state.
    pub fn new(initial: bool) -> Self {
        Self {
            registered: std::sync::Arc::new(std::sync::Mutex::new(initial)),
        }
    }
}

impl RegistryBackend for MockStartupRegistry {
    fn is_registered(&self) -> Result<bool, SyncError> {
        let val = self
            .registered
            .lock()
            .map_err(|_| SyncError::lock_poison("Mock registry lock poisoned"))?;
        Ok(*val)
    }

    fn register(&self) -> Result<(), SyncError> {
        let mut val = self
            .registered
            .lock()
            .map_err(|_| SyncError::lock_poison("Mock registry lock poisoned"))?;
        *val = true;
        Ok(())
    }

    fn unregister(&self) -> Result<(), SyncError> {
        let mut val = self
            .registered
            .lock()
            .map_err(|_| SyncError::lock_poison("Mock registry lock poisoned"))?;
        *val = false;
        Ok(())
    }
}

impl RegistryBackend for StartupRegistry {
    fn is_registered(&self) -> Result<bool, SyncError> {
        Self::is_registered()
    }

    fn register(&self) -> Result<(), SyncError> {
        Self::register()
    }

    fn unregister(&self) -> Result<(), SyncError> {
        Self::unregister()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    #[ignore = "Mutates host Windows Registry Run key; execute only in dedicated integration environments"]
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

    #[test]
    fn test_mock_startup_registry() {
        let mock = MockStartupRegistry::new(false);
        assert!(!mock.is_registered().unwrap());

        mock.register().unwrap();
        assert!(mock.is_registered().unwrap());

        mock.unregister().unwrap();
        assert!(!mock.is_registered().unwrap());
    }
}

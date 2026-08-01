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
            .map_err(|e| SyncError::LockPoison(e.to_string()))?;
        Ok(*val)
    }

    fn register(&self) -> Result<(), SyncError> {
        let mut val = self
            .registered
            .lock()
            .map_err(|e| SyncError::LockPoison(e.to_string()))?;
        *val = true;
        Ok(())
    }

    fn unregister(&self) -> Result<(), SyncError> {
        let mut val = self
            .registered
            .lock()
            .map_err(|e| SyncError::LockPoison(e.to_string()))?;
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

/// Structure containing OS and environment diagnostic information for troubleshooting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemDiagnosticInfo {
    /// Operating system product name and build (e.g. "Windows 10 Pro 22H2 (Build 19045)")
    pub os_version: String,
    /// System architecture (e.g. "x86_64")
    pub arch: String,
    /// Computer hostname if available
    pub hostname: String,
    /// Current execution username if available
    pub username: String,
    /// Application version
    pub app_version: String,
}

impl SystemDiagnosticInfo {
    /// Queries the OS and environment to gather diagnostic details.
    ///
    /// This method is infallible and degrades gracefully with fallback strings if registry
    /// or environment variable queries fail.
    pub fn collect() -> Self {
        #[cfg(windows)]
        let os_version = {
            use winreg::RegKey;
            use winreg::enums::HKEY_LOCAL_MACHINE;
            let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
            if let Ok(key) = hklm.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
                let product_name: String = key
                    .get_value("ProductName")
                    .unwrap_or_else(|_| "Windows".to_string());
                let display_version: String = key
                    .get_value("DisplayVersion")
                    .or_else(|_| key.get_value("ReleaseId"))
                    .unwrap_or_default();
                let build_number: String = key.get_value("CurrentBuildNumber").unwrap_or_default();

                let mut version_str = product_name;
                if !display_version.is_empty() {
                    version_str.push(' ');
                    version_str.push_str(&display_version);
                }
                if !build_number.is_empty() {
                    version_str.push_str(&format!(" (Build {build_number})"));
                }
                version_str
            } else {
                format!("Windows ({})", std::env::consts::ARCH)
            }
        };

        #[cfg(not(windows))]
        let os_version = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);

        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "UnknownHost".to_string());

        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "UnknownUser".to_string());

        Self {
            os_version,
            arch: std::env::consts::ARCH.to_string(),
            hostname,
            username,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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

    #[test]
    fn test_mock_startup_registry() {
        let mock = MockStartupRegistry::new(false);
        assert!(!mock.is_registered().unwrap());

        mock.register().unwrap();
        assert!(mock.is_registered().unwrap());

        mock.unregister().unwrap();
        assert!(!mock.is_registered().unwrap());
    }

    #[test]
    fn test_system_diagnostic_info_collect() {
        let info = SystemDiagnosticInfo::collect();
        assert!(
            !info.os_version.is_empty(),
            "os_version should not be empty"
        );
        assert!(!info.arch.is_empty(), "arch should not be empty");
        assert!(!info.hostname.is_empty(), "hostname should not be empty");
        assert!(!info.username.is_empty(), "username should not be empty");
        assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
    }
}

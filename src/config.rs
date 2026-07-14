//! Configuration loading and validation for syncdir.
//!
//! Parses `config.toml` and validates that source/destination directories
//! exist and runtime parameters are sane.

use crate::error::SyncError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Runtime configuration for the sync daemon.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub source_dir: PathBuf,
    pub dest_dir: PathBuf,
    pub debounce_seconds: u64,
    pub propagate_deletions: bool,
    pub block_sync_threshold_bytes: u64,
    pub block_size_bytes: u64,
    pub verify_writes: bool,
}

impl Config {
    /// Load configuration from a TOML file at the given path.
    ///
    /// # Errors
    /// Returns `SyncError::Io` if the file cannot be read, or
    /// `SyncError::Config` if the TOML content is malformed.
    pub fn load(path: &Path) -> Result<Self, SyncError> {
        let content = std::fs::read_to_string(path)?;
        let config: Config =
            toml::from_str(&content).map_err(|e| SyncError::Config(e.to_string()))?;
        Ok(config)
    }

    /// Validate that configured directories exist and parameters are valid.
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if source directory does not exist,
    /// is not a directory, or debounce is zero.
    pub fn validate(&self) -> Result<(), SyncError> {
        if !self.source_dir.exists() {
            return Err(SyncError::Validation(
                "Source directory does not exist".into(),
            ));
        }
        if !self.source_dir.is_dir() {
            return Err(SyncError::Validation(
                "Source path is not a directory".into(),
            ));
        }
        if self.debounce_seconds == 0 {
            return Err(SyncError::Validation(
                "Debounce seconds must be greater than zero".into(),
            ));
        }
        Ok(())
    }

    /// Return the default application data directory: `%APPDATA%\syncdir\`.
    ///
    /// # Errors
    /// Returns `SyncError::Config` if the `APPDATA` environment variable is not set.
    pub fn default_app_dir() -> Result<PathBuf, SyncError> {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| SyncError::Config("APPDATA environment variable not set".into()))?;
        Ok(PathBuf::from(appdata).join("syncdir"))
    }

    /// Return the default configuration file path: `%APPDATA%\syncdir\config.toml`.
    ///
    /// # Errors
    /// Returns `SyncError::Config` if the `APPDATA` environment variable is not set.
    pub fn default_config_path() -> Result<PathBuf, SyncError> {
        Ok(Self::default_app_dir()?.join("config.toml"))
    }
}

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
    /// Checks if the startup registration exists and matches the current exe.
    ///
    /// # Errors
    /// Returns `SyncError::Io` if retrieving the current executable path fails.
    pub fn is_registered() -> Result<bool, SyncError> {
        Ok(false)
    }
    /// Registers the current exe path in HKCU run key with --autostart flag.
    ///
    /// # Errors
    /// Returns `SyncError::Config` if registry write operations fail.
    pub fn register() -> Result<(), SyncError> {
        Ok(())
    }
    /// Removes the syncdir value from HKCU run key.
    pub fn unregister() -> Result<(), SyncError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_config_validation_valid() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let config = Config {
            source_dir: source,
            dest_dir: dest,
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_missing_source() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let config = Config {
            source_dir: temp.path().join("nonexistent"),
            dest_dir: dest,
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_debounce() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config {
            source_dir: source,
            dest_dir: temp.path().join("dest"),
            debounce_seconds: 0,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_default_app_dir_returns_appdata_path() {
        let dir = Config::default_app_dir().unwrap();
        let dir_str = dir.to_string_lossy().to_lowercase();
        assert!(
            dir_str.contains("appdata"),
            "Expected AppData in path, got: {dir_str}"
        );
        assert!(
            dir_str.ends_with("syncdir"),
            "Expected path to end with 'syncdir', got: {dir_str}"
        );
    }

    #[test]
    fn test_default_config_path() {
        let path = Config::default_config_path().unwrap();
        let path_str = path.to_string_lossy().to_lowercase();
        assert!(
            path_str.contains("appdata"),
            "Expected AppData in path, got: {path_str}"
        );
        assert!(
            path_str.ends_with("syncdir\\config.toml") || path_str.ends_with("syncdir/config.toml"),
            "Expected path to end with 'syncdir/config.toml', got: {path_str}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_startup_registration_toggle() {
        // Drop guard to guarantee state restoration even if test panics
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

        // Force unregister to start clean
        StartupRegistry::unregister().unwrap();
        assert!(!StartupRegistry::is_registered().unwrap());

        // Test register (value includes --autostart suffix)
        StartupRegistry::register().unwrap();
        assert!(StartupRegistry::is_registered().unwrap());

        // Test unregister
        StartupRegistry::unregister().unwrap();
        assert!(!StartupRegistry::is_registered().unwrap());
    }
}

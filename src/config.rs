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
}

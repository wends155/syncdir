//! Configuration loading and validation for syncdir.
//!
//! Parses `config.toml` and validates that source/destination directories
//! exist and runtime parameters are sane.

use crate::error::SyncError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn default_retry_interval() -> u64 {
    10
}

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
    #[serde(default = "default_retry_interval")]
    pub retry_interval_seconds: u64,
}

impl Config {
    /// Load configuration from a TOML file at the given path.
    ///
    /// # Errors
    /// Returns `SyncError::Io` if the file cannot be read, or
    /// `SyncError::Config` if the TOML content is malformed.
    pub fn load(path: &Path) -> Result<Self, SyncError> {
        let content = std::fs::read_to_string(path)?;
        let processed = preprocess_config_toml(&content);
        let config: Config =
            toml::from_str(&processed).map_err(|e| SyncError::Config(e.to_string()))?;
        Ok(config)
    }

    /// Validate that configured directories exist and parameters are valid.
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if parameters are invalid.
    pub fn validate(&self) -> Result<(), SyncError> {
        if !self.source_dir.exists() {
            tracing::warn!(
                path = %self.source_dir.display(),
                "Source directory does not exist at validation, starting in degraded mode"
            );
        } else if !self.source_dir.is_dir() {
            return Err(SyncError::Validation(
                "Source path is not a directory".into(),
            ));
        }
        if self.debounce_seconds == 0 {
            return Err(SyncError::Validation(
                "Debounce seconds must be greater than zero".into(),
            ));
        }
        if self.retry_interval_seconds == 0 {
            return Err(SyncError::Validation(
                "Retry interval seconds must be greater than zero".into(),
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

fn preprocess_config_toml(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    for line in content.lines() {
        let trimmed = line.trim();
        if (trimmed.starts_with("source_dir") || trimmed.starts_with("dest_dir"))
            && trimmed.contains('=')
        {
            let first_quote = line.find('"');
            let last_quote = line.rfind('"');
            if let Some((first, last)) = first_quote.zip(last_quote).filter(|&(f, l)| f < l) {
                let prefix = &line[..=first];
                let suffix = &line[last..];
                let path_part = &line[first + 1..last];

                let mut processed_path = String::with_capacity(path_part.len() * 2);
                let mut chars = path_part.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        if chars.peek() == Some(&'\\') {
                            processed_path.push('\\');
                            processed_path.push('\\');
                            chars.next();
                        } else {
                            processed_path.push('\\');
                            processed_path.push('\\');
                        }
                    } else {
                        processed_path.push(c);
                    }
                }
                result.push_str(prefix);
                result.push_str(&processed_path);
                result.push_str(suffix);
                result.push('\n');
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    result
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
            retry_interval_seconds: 10,
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
            retry_interval_seconds: 10,
        };
        // Soft validation: missing source directory logs a warning but validation passes
        assert!(config.validate().is_ok());
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
            retry_interval_seconds: 10,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_retry_interval() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config {
            source_dir: source,
            dest_dir: temp.path().join("dest"),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 0,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_retry_interval_default() {
        let toml_str = r#"
            source_dir = "C:\\source"
            dest_dir = "C:\\dest"
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 1024
            block_size_bytes = 512
            verify_writes = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.retry_interval_seconds, 10);
    }

    #[test]
    fn test_config_parsing_unescaped_backslashes() {
        let toml_str = r#"
            source_dir = "Y:\Mill Processing\COMMON\MAINTENANCE"
            dest_dir = "Z:\Backup\Folder"
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 1024
            block_size_bytes = 512
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(toml_str);
        let config: Config = toml::from_str(&processed).unwrap();
        assert_eq!(
            config.source_dir.to_string_lossy(),
            r#"Y:\Mill Processing\COMMON\MAINTENANCE"#
        );
        assert_eq!(config.dest_dir.to_string_lossy(), r#"Z:\Backup\Folder"#);
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
}

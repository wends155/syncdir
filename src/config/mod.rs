//! Configuration loading and validation for syncdir.
//!
//! Parses `config.toml` and validates that source/destination directories
//! exist and runtime parameters are sane.

use crate::error::SyncError;
use crate::path_util::is_same_or_descendant;
#[cfg(test)]
use crate::path_util::normalize_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) mod validation;
use validation::preprocess_config_toml;

pub(crate) mod raw;
use raw::RawConfig;

pub mod target;
pub(crate) use target::TargetRole;
pub use target::{DestinationCollection, TargetDir, VerificationMode};

pub mod builder;
pub use builder::{ConfigBuilder, TargetSyncConfigBuilder};

/// Default block size (64KB) as a non-zero integer.
pub const DEFAULT_BLOCK_SIZE: std::num::NonZeroU64 = match std::num::NonZeroU64::new(64 * 1024) {
    Some(v) => v,
    None => unreachable!(),
};

/// Isolated target sync configuration for a specific destination directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSyncConfig {
    source_dir: PathBuf,
    dest_dir: TargetDir,
    block_size_bytes: u64,
    block_sync_threshold_bytes: u64,
    verify_writes: bool,
    verification_mode: VerificationMode,
    debounce_seconds: u64,
    retry_interval_seconds: u64,
    propagate_deletions: bool,
}

impl TargetSyncConfig {
    /// Return a builder for `TargetSyncConfig`.
    ///
    /// # Arguments
    ///
    /// * `source_dir` - Path to the source directory to mirror.
    /// * `dest_dir` - Destination target directory.
    ///
    /// # Returns
    ///
    /// A configured [`TargetSyncConfigBuilder`] instance with default sync options.
    pub fn builder(
        source_dir: impl Into<PathBuf>,
        dest_dir: impl Into<TargetDir>,
    ) -> TargetSyncConfigBuilder {
        TargetSyncConfigBuilder::new(source_dir, dest_dir)
    }

    /// Construct a validated `TargetSyncConfig`.
    ///
    /// Validates directory paths, timeout positivity, block threshold ordering,
    /// and ensures destination is not identical to or nested within the source directory.
    ///
    /// # Arguments
    ///
    /// * `source_dir` - Path to the source directory to mirror.
    /// * `dest_dir` - Destination target directory.
    ///
    /// # Returns
    ///
    /// A validated [`TargetSyncConfig`] ready for use with [`LocalSyncEngine`](crate::sync::LocalSyncEngine).
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Validation`] if parameters, timeouts, block sizes, or paths fail validation,
    /// or if the destination directory is identical to or nested within the source directory.
    pub fn new(
        source_dir: impl Into<PathBuf>,
        dest_dir: impl Into<TargetDir>,
    ) -> Result<Self, SyncError> {
        Self::builder(source_dir, dest_dir).build()
    }

    /// Create a new `TargetSyncConfig` from a `Config` and a specific destination directory,
    /// routing through `TargetSyncConfigBuilder` to enforce path safety and parameter invariants.
    ///
    /// # Errors
    /// Returns [`SyncError::Validation`] if parameters, timeouts, block sizes, or paths fail validation,
    /// or if the destination directory is identical to or nested within the source directory.
    pub fn from_config(config: &Config, dest_dir: impl Into<TargetDir>) -> Result<Self, SyncError> {
        TargetSyncConfigBuilder::new(config.source_dir(), dest_dir)
            .block_size_bytes(config.block_size_bytes())
            .block_sync_threshold_bytes(config.block_sync_threshold_bytes())
            .verify_writes(config.verify_writes())
            .verification_mode(config.verification_mode())
            .debounce_seconds(config.debounce_seconds())
            .retry_interval_seconds(config.retry_interval_seconds())
            .propagate_deletions(config.propagate_deletions())
            .build()
    }

    /// Source directory getter.
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    /// Destination directory getter.
    pub fn dest_dir(&self) -> &Path {
        &self.dest_dir
    }

    /// Destination target dir getter.
    pub fn dest_target_dir(&self) -> &TargetDir {
        &self.dest_dir
    }

    /// Block size in bytes getter.
    pub fn block_size_bytes(&self) -> u64 {
        self.block_size_bytes
    }

    /// Returns the configured block size as a `NonZeroU64`, defaulting to 64KB if zero.
    ///
    /// # Returns
    ///
    /// A [`std::num::NonZeroU64`] representing the block size in bytes.
    #[must_use]
    pub fn block_size_nonzero(&self) -> std::num::NonZeroU64 {
        std::num::NonZeroU64::new(self.block_size_bytes).unwrap_or(DEFAULT_BLOCK_SIZE)
    }

    /// Block sync threshold in bytes getter.
    pub fn block_sync_threshold_bytes(&self) -> u64 {
        self.block_sync_threshold_bytes
    }

    /// Verify writes flag getter.
    pub fn verify_writes(&self) -> bool {
        self.verify_writes
    }

    /// Verification mode getter.
    pub fn verification_mode(&self) -> VerificationMode {
        self.verification_mode
    }

    /// Debounce seconds getter.
    pub fn debounce_seconds(&self) -> u64 {
        self.debounce_seconds
    }

    /// Retry interval in seconds getter.
    pub fn retry_interval_seconds(&self) -> u64 {
        self.retry_interval_seconds
    }

    /// Propagate deletions flag getter.
    pub fn propagate_deletions(&self) -> bool {
        self.propagate_deletions
    }

    /// Sets write verification flag.
    pub fn with_verify_writes(mut self, verify: bool) -> Self {
        self.verify_writes = verify;
        self.verification_mode = VerificationMode::from_legacy_flag(verify);
        self
    }

    /// Sets verification mode.
    pub fn with_verification_mode(mut self, mode: VerificationMode) -> Self {
        self.verification_mode = mode;
        self.verify_writes = mode != VerificationMode::Disabled;
        self
    }

    /// Explicit fallible conversion from `&Config` that returns an error if no destinations are configured.
    pub fn try_from_config(cfg: &Config) -> Result<Self, SyncError> {
        let dest = cfg.dest_dir().ok_or_else(|| {
            SyncError::validation("Config has no destination directories configured")
        })?;
        Self::from_config(cfg, dest)
    }

    /// Explicit fallible conversion from owned `Config`.
    pub fn try_from_config_owned(cfg: Config) -> Result<Self, SyncError> {
        Self::try_from_config(&cfg)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_raw_parts(
        source_dir: PathBuf,
        dest_dir: TargetDir,
        block_size_bytes: u64,
        block_sync_threshold_bytes: u64,
        verify_writes: bool,
        verification_mode: VerificationMode,
        debounce_seconds: u64,
        retry_interval_seconds: u64,
        propagate_deletions: bool,
    ) -> Self {
        Self {
            source_dir,
            dest_dir,
            block_size_bytes,
            block_sync_threshold_bytes,
            verify_writes,
            verification_mode,
            debounce_seconds,
            retry_interval_seconds,
            propagate_deletions,
        }
    }
}

/// Runtime configuration for the sync daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "RawConfig", into = "RawConfig")]
pub struct Config {
    source_dir: TargetDir,
    destinations: DestinationCollection,
    debounce_seconds: u64,
    propagate_deletions: bool,
    block_sync_threshold_bytes: u64,
    block_size_bytes: u64,
    verify_writes: bool,
    verification_mode: Option<VerificationMode>,
    retry_interval_seconds: u64,
}

impl From<RawConfig> for Config {
    fn from(raw: RawConfig) -> Self {
        Self {
            source_dir: TargetDir::new(raw.source_dir),
            destinations: DestinationCollection::from_raw(raw.dest_dir, raw.dest_dirs),
            debounce_seconds: raw.debounce_seconds,
            propagate_deletions: raw.propagate_deletions,
            block_sync_threshold_bytes: raw.block_sync_threshold_bytes,
            block_size_bytes: raw.block_size_bytes,
            verify_writes: raw.verify_writes,
            verification_mode: raw.verification_mode,
            retry_interval_seconds: raw.retry_interval_seconds,
        }
    }
}

impl From<Config> for RawConfig {
    fn from(cfg: Config) -> Self {
        let dest_dir = cfg.destinations.iter().next().map(TargetDir::to_path_buf);
        let dest_dirs = if cfg.destinations.len() > 1 {
            Some(
                cfg.destinations
                    .iter()
                    .skip(1)
                    .map(TargetDir::to_path_buf)
                    .collect(),
            )
        } else {
            None
        };
        Self {
            source_dir: cfg.source_dir.to_path_buf(),
            dest_dir,
            dest_dirs,
            debounce_seconds: cfg.debounce_seconds,
            propagate_deletions: cfg.propagate_deletions,
            block_sync_threshold_bytes: cfg.block_sync_threshold_bytes,
            block_size_bytes: cfg.block_size_bytes,
            verify_writes: cfg.verify_writes,
            verification_mode: cfg.verification_mode,
            retry_interval_seconds: cfg.retry_interval_seconds,
        }
    }
}

impl Config {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_raw_parts(
        source_dir: TargetDir,
        destinations: DestinationCollection,
        debounce_seconds: u64,
        propagate_deletions: bool,
        block_sync_threshold_bytes: u64,
        block_size_bytes: u64,
        verify_writes: bool,
        verification_mode: Option<VerificationMode>,
        retry_interval_seconds: u64,
    ) -> Self {
        Self {
            source_dir,
            destinations,
            debounce_seconds,
            propagate_deletions,
            block_sync_threshold_bytes,
            block_size_bytes,
            verify_writes,
            verification_mode,
            retry_interval_seconds,
        }
    }

    /// Return builder initialized with source directory.
    pub fn builder(source_dir: impl Into<PathBuf>) -> ConfigBuilder {
        ConfigBuilder::new(source_dir)
    }

    /// Return a clone of this Config with the specified destination directory set.
    pub fn with_dest_dir(&self, dest: PathBuf) -> Self {
        let mut cloned = self.clone();
        cloned.destinations = DestinationCollection::from_raw(Some(dest), None);
        cloned
    }

    /// Source directory getter.
    pub fn source_dir(&self) -> &Path {
        self.source_dir.as_path()
    }

    /// Return strongly-typed source TargetDir.
    pub fn source_target_dir(&self) -> &TargetDir {
        &self.source_dir
    }

    /// Return the normalized source directory path.
    #[deprecated(since = "0.2.0", note = "Use source_dir() directly")]
    pub fn resolved_source_dir(&self) -> &Path {
        self.source_dir.as_path()
    }

    /// Return strongly-typed destination slice.
    pub fn destinations(&self) -> &[TargetDir] {
        self.destinations.as_slice()
    }

    /// Return a merged, deduplicated list of all configured destination directories.
    pub fn resolved_dest_dirs(&self) -> Vec<PathBuf> {
        self.destinations.to_path_bufs()
    }

    /// Primary destination directory getter.
    pub fn dest_dir(&self) -> Option<&Path> {
        self.destinations.iter().next().map(TargetDir::as_path)
    }

    /// Extra destination directories getter.
    pub fn dest_dirs(&self) -> Option<Vec<PathBuf>> {
        if self.destinations.is_empty() {
            None
        } else {
            Some(self.destinations.to_path_bufs())
        }
    }

    /// Debounce seconds getter.
    pub fn debounce_seconds(&self) -> u64 {
        self.debounce_seconds
    }

    /// Propagate deletions flag getter.
    pub fn propagate_deletions(&self) -> bool {
        self.propagate_deletions
    }

    /// Block sync threshold in bytes getter.
    pub fn block_sync_threshold_bytes(&self) -> u64 {
        self.block_sync_threshold_bytes
    }

    /// Block size in bytes getter.
    pub fn block_size_bytes(&self) -> u64 {
        self.block_size_bytes
    }

    /// Write verification flag getter.
    pub fn verify_writes(&self) -> bool {
        self.verify_writes
    }

    /// Return the resolved verification mode.
    ///
    /// If `verification_mode` was explicitly configured, returns it.
    /// Otherwise, resolves based on the legacy `verify_writes` boolean flag.
    pub fn verification_mode(&self) -> VerificationMode {
        self.verification_mode
            .unwrap_or_else(|| VerificationMode::from_legacy_flag(self.verify_writes))
    }

    /// Retry interval in seconds getter.
    pub fn retry_interval_seconds(&self) -> u64 {
        self.retry_interval_seconds
    }

    /// Generate isolated target sync configurations for each configured destination directory.
    ///
    /// # Errors
    /// Returns [`SyncError::Validation`] if any target configuration fails invariant validation.
    pub fn target_configs(&self) -> Result<Vec<TargetSyncConfig>, SyncError> {
        self.destinations
            .iter()
            .map(|dest| TargetSyncConfig::from_config(self, dest.clone()))
            .collect()
    }

    /// Create a Config with sensible test defaults for the given source and dest.
    #[doc(hidden)]
    pub fn test_default(source: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        ConfigBuilder::new(source)
            .dest_dir(dest)
            .debounce_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(10)
            .block_size_bytes(4)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build_unvalidated()
    }

    /// Load configuration from a TOML file at the given path.
    /// Automatically normalizes all configured directory paths upon loading.
    ///
    /// # Errors
    /// Returns `SyncError::Io` if the file cannot be read, or
    /// `SyncError::Config` if the TOML content is malformed.
    pub fn load(path: &Path) -> Result<Self, SyncError> {
        let content = std::fs::read_to_string(path)?;
        let processed = preprocess_config_toml(&content);
        let config: Config = toml::from_str(&processed)?;
        Ok(config)
    }

    /// Validate that configured directories exist and parameters are valid.
    /// Enforces that source and destination paths are valid drive paths (`C:\`) or UNC network paths (`\\`).
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if parameters are invalid.
    pub fn validate(&self) -> Result<(), SyncError> {
        self.source_dir.validate(TargetRole::Source)?;

        if !self.source_dir.exists() {
            tracing::warn!(
                path = %self.source_dir.display(),
                "Source directory does not exist at validation, starting in degraded mode"
            );
        } else if !self.source_dir.is_dir() {
            return Err(SyncError::validation("Source path is not a directory"));
        }

        if self.destinations.is_empty() {
            return Err(SyncError::validation(
                "At least one destination directory must be specified (via dest_dir or dest_dirs)",
            ));
        }

        for dest in self.destinations.iter() {
            dest.validate(TargetRole::Destination)?;
            if is_same_or_descendant(self.source_dir.as_path(), dest.as_path())
                || is_same_or_descendant(dest.as_path(), self.source_dir.as_path())
            {
                return Err(SyncError::validation_loop(format!(
                    "Destination directory '{}' is identical to or nested within source directory '{}' (recursive sync loop)",
                    dest.display(),
                    self.source_dir.display()
                )));
            }
        }

        // Validate that no two destination directories overlap or nest within each other
        let dests: Vec<&Path> = self.destinations.iter().map(|d| d.as_path()).collect();
        for i in 0..dests.len() {
            for j in (i + 1)..dests.len() {
                if is_same_or_descendant(dests[i], dests[j])
                    || is_same_or_descendant(dests[j], dests[i])
                {
                    return Err(SyncError::validation_loop(format!(
                        "Destination directories '{}' and '{}' are identical or nested within each other",
                        dests[i].display(),
                        dests[j].display()
                    )));
                }
            }
        }

        if self.debounce_seconds == 0 {
            return Err(SyncError::validation_invariant(
                "Debounce seconds must be greater than zero",
            ));
        }
        if self.retry_interval_seconds == 0 {
            return Err(SyncError::validation_invariant(
                "Retry interval seconds must be greater than zero",
            ));
        }
        if self.block_size_bytes == 0 {
            return Err(SyncError::validation_invariant(
                "block_size_bytes must be greater than zero",
            ));
        }
        if self.block_size_bytes > validation::MAX_BLOCK_SIZE_BYTES {
            return Err(SyncError::validation_invariant(
                "block_size_bytes must not exceed 64MB",
            ));
        }
        if self.block_sync_threshold_bytes == 0 {
            return Err(SyncError::validation_invariant(
                "block_sync_threshold_bytes must be greater than zero",
            ));
        }
        if self.block_sync_threshold_bytes < self.block_size_bytes {
            return Err(SyncError::validation_invariant(
                "block_sync_threshold_bytes must be greater than or equal to block_size_bytes",
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
            .map_err(|_| SyncError::config("APPDATA environment variable not set"))?;
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

impl TryFrom<&Config> for crate::db::StoreConfig {
    type Error = SyncError;

    fn try_from(config: &Config) -> Result<Self, Self::Error> {
        Self::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
    }
}

impl TryFrom<&TargetSyncConfig> for crate::db::StoreConfig {
    type Error = SyncError;

    fn try_from(config: &TargetSyncConfig) -> Result<Self, Self::Error> {
        Self::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
    }
}

#[cfg(test)]
mod tests;

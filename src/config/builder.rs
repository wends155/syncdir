//! Fluent configuration builders for syncdir configuration and sync targets.

use crate::config::target::{DestinationCollection, TargetDir, TargetRole, VerificationMode};
use crate::config::validation;
use crate::config::{Config, TargetSyncConfig};
use crate::error::SyncError;
use crate::path_util::is_same_or_descendant;
use std::path::PathBuf;

/// Builder for constructing and validating a [`TargetSyncConfig`].
#[derive(Debug, Clone)]
pub struct TargetSyncConfigBuilder {
    source_dir: PathBuf,
    dest_dir: TargetDir,
    block_size_bytes: u64,
    block_sync_threshold_bytes: u64,
    verify_writes: bool,
    verification_mode: Option<VerificationMode>,
    debounce_seconds: u64,
    retry_interval_seconds: u64,
    propagate_deletions: bool,
}

impl TargetSyncConfigBuilder {
    /// Create a new builder with default operational settings.
    ///
    /// # Arguments
    ///
    /// * `source_dir` - Path to the source directory to mirror.
    /// * `dest_dir` - Destination target directory.
    ///
    /// # Returns
    ///
    /// A [`TargetSyncConfigBuilder`] initialized with 1MB blocks, 10MB threshold, 3s debounce, and 10s retry.
    pub fn new(source_dir: impl Into<PathBuf>, dest_dir: impl Into<TargetDir>) -> Self {
        Self {
            source_dir: source_dir.into(),
            dest_dir: dest_dir.into(),
            block_size_bytes: 1024 * 1024,
            block_sync_threshold_bytes: 10 * 1024 * 1024,
            verify_writes: true,
            verification_mode: None,
            debounce_seconds: validation::DEFAULT_DEBOUNCE_SECONDS,
            retry_interval_seconds: 10,
            propagate_deletions: true,
        }
    }

    /// Set block size in bytes.
    pub fn block_size_bytes(mut self, val: u64) -> Self {
        self.block_size_bytes = val;
        self
    }

    /// Set block sync threshold in bytes.
    pub fn block_sync_threshold_bytes(mut self, val: u64) -> Self {
        self.block_sync_threshold_bytes = val;
        self
    }

    /// Set write verification flag.
    pub fn verify_writes(mut self, val: bool) -> Self {
        self.verify_writes = val;
        self
    }

    /// Set verification mode.
    pub fn verification_mode(mut self, mode: VerificationMode) -> Self {
        self.verification_mode = Some(mode);
        self.verify_writes = mode != VerificationMode::Disabled;
        self
    }

    /// Set debouncing interval in seconds.
    pub fn debounce_seconds(mut self, val: u64) -> Self {
        self.debounce_seconds = val;
        self
    }

    /// Set retry interval in seconds.
    pub fn retry_interval_seconds(mut self, val: u64) -> Self {
        self.retry_interval_seconds = val;
        self
    }

    /// Set deletion propagation flag.
    pub fn propagate_deletions(mut self, val: bool) -> Self {
        self.propagate_deletions = val;
        self
    }

    /// Builds and validates the `TargetSyncConfig`.
    ///
    /// Validates directory paths, timeout positivity, block threshold ordering,
    /// and ensures destination is not identical to or nested within the source directory.
    ///
    /// # Returns
    ///
    /// A validated [`TargetSyncConfig`] ready for sync worker execution.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Validation`] if parameters, timeouts, block sizes, or paths fail validation,
    /// or if the destination directory is identical to or nested within the source directory.
    pub fn build(self) -> Result<TargetSyncConfig, SyncError> {
        let max_block_size = validation::MAX_BLOCK_SIZE_BYTES;
        if self.block_size_bytes == 0 || self.block_size_bytes > max_block_size {
            return Err(SyncError::validation(format!(
                "block_size_bytes must be between 1 and {max_block_size}, got {}",
                self.block_size_bytes
            )));
        }
        if self.block_sync_threshold_bytes == 0 {
            return Err(SyncError::validation(
                "block_sync_threshold_bytes must be greater than zero",
            ));
        }
        if self.block_sync_threshold_bytes < self.block_size_bytes {
            return Err(SyncError::validation(
                "block_sync_threshold_bytes must be greater than or equal to block_size_bytes",
            ));
        }
        if self.debounce_seconds == 0 {
            return Err(SyncError::validation(
                "debounce_seconds must be greater than zero",
            ));
        }
        if self.retry_interval_seconds == 0 {
            return Err(SyncError::validation(
                "retry_interval_seconds must be greater than zero",
            ));
        }
        let src_target = TargetDir::new(&self.source_dir);
        src_target.validate(TargetRole::Source)?;
        self.dest_dir.validate(TargetRole::Destination)?;

        if is_same_or_descendant(src_target.as_path(), self.dest_dir.as_path())
            || is_same_or_descendant(self.dest_dir.as_path(), src_target.as_path())
        {
            return Err(SyncError::validation_loop(format!(
                "Destination directory '{}' is identical to or nested within source directory '{}' (recursive sync loop)",
                self.dest_dir.display(),
                src_target.display()
            )));
        }

        let verification_mode = self
            .verification_mode
            .unwrap_or_else(|| VerificationMode::from_legacy_flag(self.verify_writes));
        Ok(TargetSyncConfig::from_raw_parts(
            src_target.to_path_buf(),
            self.dest_dir,
            self.block_size_bytes,
            self.block_sync_threshold_bytes,
            self.verify_writes,
            verification_mode,
            self.debounce_seconds,
            self.retry_interval_seconds,
            self.propagate_deletions,
        ))
    }
}

/// Builder for creating and customizing [`Config`] instances.
#[derive(Debug, Clone)]
pub struct ConfigBuilder {
    source_dir: PathBuf,
    dest_dir: Option<PathBuf>,
    debounce_seconds: u64,
    propagate_deletions: bool,
    block_sync_threshold_bytes: u64,
    block_size_bytes: u64,
    verify_writes: bool,
    verification_mode: Option<VerificationMode>,
    retry_interval_seconds: u64,
    dest_dirs: Option<Vec<PathBuf>>,
}

impl ConfigBuilder {
    /// Create a new builder with the given source directory and default values.
    pub fn new(source_dir: impl Into<PathBuf>) -> Self {
        Self {
            source_dir: source_dir.into(),
            dest_dir: None,
            debounce_seconds: validation::DEFAULT_DEBOUNCE_SECONDS,
            propagate_deletions: true,
            block_sync_threshold_bytes: 10 * 1024 * 1024,
            block_size_bytes: 1024 * 1024,
            verify_writes: true,
            verification_mode: None,
            retry_interval_seconds: validation::DEFAULT_RETRY_INTERVAL_SECONDS,
            dest_dirs: None,
        }
    }

    /// Set primary destination directory.
    pub fn dest_dir(mut self, dest: impl Into<PathBuf>) -> Self {
        self.dest_dir = Some(dest.into());
        self
    }

    /// Set multiple destination directories.
    pub fn dest_dirs(mut self, dirs: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.dest_dirs = Some(dirs.into_iter().map(Into::into).collect());
        self
    }

    /// Set debouncing duration in seconds.
    pub fn debounce_seconds(mut self, val: u64) -> Self {
        self.debounce_seconds = val;
        self
    }

    /// Set whether file deletions should propagate.
    pub fn propagate_deletions(mut self, val: bool) -> Self {
        self.propagate_deletions = val;
        self
    }

    /// Set block sync threshold in bytes.
    pub fn block_sync_threshold_bytes(mut self, val: u64) -> Self {
        self.block_sync_threshold_bytes = val;
        self
    }

    /// Set block size in bytes.
    pub fn block_size_bytes(mut self, val: u64) -> Self {
        self.block_size_bytes = val;
        self
    }

    /// Set write verification flag.
    pub fn verify_writes(mut self, val: bool) -> Self {
        self.verify_writes = val;
        self
    }

    /// Set write verification mode explicitly.
    pub fn verification_mode(mut self, mode: VerificationMode) -> Self {
        self.verification_mode = Some(mode);
        self.verify_writes = mode != VerificationMode::Disabled;
        self
    }

    /// Set retry interval in seconds.
    pub fn retry_interval_seconds(mut self, val: u64) -> Self {
        self.retry_interval_seconds = val;
        self
    }

    /// Add an additional destination directory.
    pub fn add_dest_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        match self.dest_dirs.as_mut() {
            Some(dirs) => dirs.push(dir),
            None => self.dest_dirs = Some(vec![dir]),
        }
        self
    }

    /// Builds and validates the configuration into a [`Config`].
    ///
    /// Validates all configuration invariants including non-zero debounce and retry
    /// intervals, valid block size bounds (1 byte to 64MB), block threshold ordering,
    /// destination presence, and recursive path containment.
    ///
    /// # Returns
    ///
    /// A fully validated [`Config`] ready for synchronization.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Validation`] if any configuration invariant is violated:
    /// - `debounce_seconds == 0`
    /// - `retry_interval_seconds == 0`
    /// - `block_size_bytes == 0` or `> 64MB`
    /// - `block_sync_threshold_bytes < block_size_bytes`
    /// - No destination directories specified
    /// - Source and destination directories have recursive containment (`source == dest` or nested)
    ///
    /// # Examples
    ///
    /// ```rust
    /// use syncdir::config::Config;
    ///
    /// # fn main() -> Result<(), syncdir::error::SyncError> {
    /// let config = Config::builder("C:\\Source")
    ///     .dest_dir("D:\\Dest")
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn build(self) -> Result<Config, SyncError> {
        let config = self.build_unvalidated();
        config.validate()?;
        Ok(config)
    }

    /// Builds configuration without running invariant validation.
    ///
    /// Normalizes configured paths without asserting invariants, allowing
    /// [`Config::validate`] to be invoked explicitly or permitting negative test fixtures.
    ///
    /// # Returns
    ///
    /// An unvalidated [`Config`].
    #[doc(hidden)]
    pub fn build_unvalidated(self) -> Config {
        Config::from_raw_parts(
            TargetDir::new(self.source_dir),
            DestinationCollection::from_raw(self.dest_dir, self.dest_dirs),
            self.debounce_seconds,
            self.propagate_deletions,
            self.block_sync_threshold_bytes,
            self.block_size_bytes,
            self.verify_writes,
            self.verification_mode,
            self.retry_interval_seconds,
        )
    }

    /// Fallible builder method equivalent to [`ConfigBuilder::build`].
    ///
    /// # Returns
    ///
    /// A fully validated [`Config`].
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Validation`] if validation invariants fail.
    #[inline]
    pub fn try_build(self) -> Result<Config, SyncError> {
        self.build()
    }
}

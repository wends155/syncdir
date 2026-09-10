//! Configuration loading and validation for syncdir.
//!
//! Parses `config.toml` and validates that source/destination directories
//! exist and runtime parameters are sane.

use crate::error::SyncError;
use crate::path_util::normalize_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn default_retry_interval() -> u64 {
    10
}

/// Write verification strategy for synced files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    /// Skip write verification entirely.
    Disabled,
    /// Verify file size metadata and issue fsync/flush (default).
    #[default]
    MetadataAndFlush,
    /// Sampled block verification (first, last, stratified interior blocks).
    Sampled,
    /// Full block readback verification (legacy verify_writes = true).
    Full,
}

impl VerificationMode {
    /// Map legacy boolean `verify_writes` flag to a `VerificationMode`.
    #[must_use]
    pub const fn from_legacy_flag(verify_writes: bool) -> Self {
        if verify_writes {
            Self::Full
        } else {
            Self::Disabled
        }
    }
}

/// Isolated target sync configuration for a specific destination directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSyncConfig {
    pub(crate) source_dir: PathBuf,
    pub(crate) dest_dir: TargetDir,
    pub(crate) block_size_bytes: u64,
    pub(crate) block_sync_threshold_bytes: u64,
    pub(crate) verify_writes: bool,
    pub(crate) verification_mode: VerificationMode,
    pub(crate) debounce_seconds: u64,
    pub(crate) retry_interval_seconds: u64,
    pub(crate) propagate_deletions: bool,
}

impl TargetSyncConfig {
    /// Return a builder for `TargetSyncConfig`.
    pub fn builder(
        source_dir: impl Into<PathBuf>,
        dest_dir: impl Into<TargetDir>,
    ) -> TargetSyncConfigBuilder {
        TargetSyncConfigBuilder::new(source_dir, dest_dir)
    }

    /// Construct a validated `TargetSyncConfig`.
    pub fn new(
        source_dir: impl Into<PathBuf>,
        dest_dir: impl Into<TargetDir>,
    ) -> Result<Self, SyncError> {
        Self::builder(source_dir, dest_dir).build()
    }

    /// Create a new `TargetSyncConfig` from a `Config` and a specific destination directory.
    pub fn from_config(config: &Config, dest_dir: impl Into<TargetDir>) -> Self {
        Self {
            source_dir: config.source_dir().to_path_buf(),
            dest_dir: dest_dir.into(),
            block_size_bytes: config.block_size_bytes(),
            block_sync_threshold_bytes: config.block_sync_threshold_bytes(),
            verify_writes: config.verify_writes(),
            verification_mode: config.verification_mode(),
            debounce_seconds: config.debounce_seconds(),
            retry_interval_seconds: config.retry_interval_seconds(),
            propagate_deletions: config.propagate_deletions(),
        }
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
        std::num::NonZeroU64::new(self.block_size_bytes)
            .unwrap_or_else(|| std::num::NonZeroU64::new(64 * 1024).expect("64KB is non-zero"))
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
        Ok(Self::from_config(cfg, dest))
    }

    /// Explicit fallible conversion from owned `Config`.
    pub fn try_from_config_owned(cfg: Config) -> Result<Self, SyncError> {
        Self::try_from_config(&cfg)
    }
}

/// Builder for constructing and validating a `TargetSyncConfig`.
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
    pub fn new(source_dir: impl Into<PathBuf>, dest_dir: impl Into<TargetDir>) -> Self {
        Self {
            source_dir: source_dir.into(),
            dest_dir: dest_dir.into(),
            block_size_bytes: 1024 * 1024,
            block_sync_threshold_bytes: 10 * 1024 * 1024,
            verify_writes: true,
            verification_mode: None,
            debounce_seconds: 3,
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
    /// # Errors
    /// Returns `SyncError::Validation` if parameters or paths fail validation.
    pub fn build(self) -> Result<TargetSyncConfig, SyncError> {
        let max_block_size = 64 * 1024 * 1024;
        if self.block_size_bytes == 0 || self.block_size_bytes > max_block_size {
            return Err(SyncError::validation(format!(
                "block_size_bytes must be between 1 and {max_block_size}, got {}",
                self.block_size_bytes
            )));
        }
        if self.block_sync_threshold_bytes == 0 {
            return Err(SyncError::validation(
                "block_sync_threshold_bytes must be greater than 0",
            ));
        }
        let src_target = TargetDir::new(&self.source_dir);
        src_target.validate(TargetRole::Source)?;
        self.dest_dir.validate(TargetRole::Destination)?;
        let verification_mode = self
            .verification_mode
            .unwrap_or_else(|| VerificationMode::from_legacy_flag(self.verify_writes));
        Ok(TargetSyncConfig {
            source_dir: src_target.to_path_buf(),
            dest_dir: self.dest_dir,
            block_size_bytes: self.block_size_bytes,
            block_sync_threshold_bytes: self.block_sync_threshold_bytes,
            verify_writes: self.verify_writes,
            verification_mode,
            debounce_seconds: self.debounce_seconds,
            retry_interval_seconds: self.retry_interval_seconds,
            propagate_deletions: self.propagate_deletions,
        })
    }
}

/// Role of a synchronized target directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetRole {
    Source,
    Destination,
}

impl TargetRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Destination => "destination",
        }
    }
}

impl std::fmt::Display for TargetRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Strongly-typed, normalized synchronization root directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "PathBuf", into = "PathBuf")]
pub struct TargetDir(PathBuf);

impl TargetDir {
    /// Construct TargetDir by normalizing path via normalize_path(). Infallible.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(normalize_path(path.into()))
    }

    /// Validates path syntax for a given role (`TargetRole::Source` or `TargetRole::Destination`).
    /// Accepts Windows drive letters (C:\), UNC prefixes (\\), and Unix absolute paths (/).
    pub fn validate(&self, role: TargetRole) -> Result<(), SyncError> {
        let is_valid_drive_path = |path_str: &str| -> bool {
            if path_str.len() < 3 {
                return false;
            }
            let bytes = path_str.as_bytes();
            bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/')
        };

        let s = self.0.to_string_lossy();
        let is_unc = crate::path_util::parse_unc_host_and_share(&self.0).is_some()
            && !s.starts_with(r"\\.\")
            && !s.starts_with(r"\\?\");
        let is_drive = is_valid_drive_path(&s);
        let is_unix_abs = s.starts_with('/');

        if is_drive {
            tracing::debug!(target_path = %s, "Validated Windows drive path target");
        }

        if !is_unc && !is_drive && !is_unix_abs {
            let example_drive = match role {
                TargetRole::Source => "C:\\, R:\\",
                TargetRole::Destination => "C:\\, X:\\",
            };
            return Err(SyncError::Validation(format!(
                "Invalid {role} path '{s}': must start with a drive letter (e.g. {example_drive}) or UNC network prefix (e.g. \\\\server\\share)"
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn to_path_buf(&self) -> PathBuf {
        self.0.clone()
    }
}

impl std::ops::Deref for TargetDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TargetDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Display for TargetDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

impl From<PathBuf> for TargetDir {
    fn from(p: PathBuf) -> Self {
        Self::new(p)
    }
}

impl From<&Path> for TargetDir {
    fn from(p: &Path) -> Self {
        Self::new(p)
    }
}

impl From<&str> for TargetDir {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<TargetDir> for PathBuf {
    fn from(td: TargetDir) -> Self {
        td.0
    }
}

impl PartialEq<PathBuf> for TargetDir {
    fn eq(&self, other: &PathBuf) -> bool {
        &self.0 == other
    }
}

impl PartialEq<TargetDir> for PathBuf {
    fn eq(&self, other: &TargetDir) -> bool {
        self == &other.0
    }
}

impl PartialEq<Path> for TargetDir {
    fn eq(&self, other: &Path) -> bool {
        self.0.as_path() == other
    }
}

impl PartialEq<TargetDir> for Path {
    fn eq(&self, other: &TargetDir) -> bool {
        self == other.0.as_path()
    }
}

impl PartialEq<&Path> for TargetDir {
    fn eq(&self, other: &&Path) -> bool {
        self.0.as_path() == *other
    }
}

impl PartialEq<TargetDir> for &Path {
    fn eq(&self, other: &TargetDir) -> bool {
        *self == other.0.as_path()
    }
}

/// Deduplicated, ordered collection of target destination directories.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationCollection {
    destinations: Vec<TargetDir>,
}

impl DestinationCollection {
    /// Create a collection from an iterator of TargetDir, deduplicating case-insensitively while preserving order.
    #[must_use]
    pub fn new(destinations: impl IntoIterator<Item = TargetDir>) -> Self {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for dest in destinations {
            let key = dest.as_path().to_string_lossy().to_lowercase();
            if seen.insert(key) {
                result.push(dest);
            }
        }
        Self {
            destinations: result,
        }
    }

    /// Construct from legacy optional primary and additional destination paths.
    #[must_use]
    pub fn from_raw(primary: Option<PathBuf>, additional: Option<Vec<PathBuf>>) -> Self {
        let mut items = Vec::new();
        if let Some(p) = primary {
            items.push(TargetDir::new(p));
        }
        if let Some(adds) = additional {
            for a in adds {
                items.push(TargetDir::new(a));
            }
        }
        Self::new(items)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TargetDir> {
        self.destinations.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.destinations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.destinations.is_empty()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[TargetDir] {
        &self.destinations
    }

    #[must_use]
    pub fn to_path_bufs(&self) -> Vec<PathBuf> {
        self.destinations
            .iter()
            .map(TargetDir::to_path_buf)
            .collect()
    }

    #[must_use]
    pub fn get(&self, idx: usize) -> Option<&TargetDir> {
        self.destinations.get(idx)
    }
}

impl std::ops::Index<usize> for DestinationCollection {
    type Output = TargetDir;
    fn index(&self, idx: usize) -> &Self::Output {
        &self.destinations[idx]
    }
}

impl FromIterator<TargetDir> for DestinationCollection {
    fn from_iter<T: IntoIterator<Item = TargetDir>>(iter: T) -> Self {
        Self::new(iter)
    }
}

impl IntoIterator for DestinationCollection {
    type Item = TargetDir;
    type IntoIter = std::vec::IntoIter<TargetDir>;
    fn into_iter(self) -> Self::IntoIter {
        self.destinations.into_iter()
    }
}

impl<'a> IntoIterator for &'a DestinationCollection {
    type Item = &'a TargetDir;
    type IntoIter = std::slice::Iter<'a, TargetDir>;
    fn into_iter(self) -> Self::IntoIter {
        self.destinations.iter()
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

#[derive(Serialize, Deserialize)]
struct RawConfig {
    source_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dest_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dest_dirs: Option<Vec<PathBuf>>,
    debounce_seconds: u64,
    propagate_deletions: bool,
    block_sync_threshold_bytes: u64,
    block_size_bytes: u64,
    verify_writes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification_mode: Option<VerificationMode>,
    #[serde(default = "default_retry_interval")]
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
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 10 * 1024 * 1024,
            block_size_bytes: 1024 * 1024,
            verify_writes: true,
            verification_mode: None,
            retry_interval_seconds: default_retry_interval(),
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

    /// Builds and normalizes paths without failing validation, allowing `config.validate()` to be called.
    pub fn build(self) -> Config {
        Config {
            source_dir: TargetDir::new(self.source_dir),
            destinations: DestinationCollection::from_raw(self.dest_dir, self.dest_dirs),
            debounce_seconds: self.debounce_seconds,
            propagate_deletions: self.propagate_deletions,
            block_sync_threshold_bytes: self.block_sync_threshold_bytes,
            block_size_bytes: self.block_size_bytes,
            verify_writes: self.verify_writes,
            verification_mode: self.verification_mode,
            retry_interval_seconds: self.retry_interval_seconds,
        }
    }

    /// Builds and validates the configuration, returning an error if validation fails.
    pub fn try_build(self) -> Result<Config, SyncError> {
        let config = self.build();
        config.validate()?;
        Ok(config)
    }
}

/// Returns true if `target` is identical to `base` or is a descendant of `base`.
///
/// Uses Windows case-insensitive component comparison with lexical component collapsing.
#[must_use]
pub fn is_same_or_descendant(base: &Path, target: &Path) -> bool {
    let base_comps = crate::path_util::collapse_components(base);
    let target_comps = crate::path_util::collapse_components(target);
    if target_comps.len() < base_comps.len() {
        return false;
    }
    base_comps.iter().zip(target_comps.iter()).all(|(b, t)| {
        b.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&t.as_os_str().to_string_lossy())
    })
}

impl Config {
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
    pub fn target_configs(&self) -> Vec<TargetSyncConfig> {
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
            .build()
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
            return Err(SyncError::Validation(
                "Source path is not a directory".into(),
            ));
        }

        if self.destinations.is_empty() {
            return Err(SyncError::Validation(
                "At least one destination directory must be specified (via dest_dir or dest_dirs)"
                    .into(),
            ));
        }

        for dest in self.destinations.iter() {
            dest.validate(TargetRole::Destination)?;
            if is_same_or_descendant(self.source_dir.as_path(), dest.as_path())
                || is_same_or_descendant(dest.as_path(), self.source_dir.as_path())
            {
                return Err(SyncError::Validation(format!(
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
                    return Err(SyncError::Validation(format!(
                        "Destination directories '{}' and '{}' are identical or nested within each other",
                        dests[i].display(),
                        dests[j].display()
                    )));
                }
            }
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
        if self.block_size_bytes == 0 {
            return Err(SyncError::Validation(
                "block_size_bytes must be greater than zero".into(),
            ));
        }
        if self.block_size_bytes > 64 * 1024 * 1024 {
            return Err(SyncError::Validation(
                "block_size_bytes must not exceed 64MB".into(),
            ));
        }
        if self.block_sync_threshold_bytes == 0 {
            return Err(SyncError::Validation(
                "block_sync_threshold_bytes must be greater than zero".into(),
            ));
        }
        if self.block_sync_threshold_bytes < self.block_size_bytes {
            return Err(SyncError::Validation(
                "block_sync_threshold_bytes must be greater than or equal to block_size_bytes"
                    .into(),
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

/// Returns the Windows system root directory (e.g. `C:\Windows`).
/// Reads `%SystemRoot%`, then `%windir%`, defaulting to `C:\Windows`.
pub fn system_root() -> PathBuf {
    std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
}

fn preprocess_config_toml(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut in_dest_dirs_array = false;

    let has_bracket_outside_quotes = |s: &str, target: char| -> bool {
        let mut in_q = false;
        for ch in s.chars() {
            if ch == '"' {
                in_q = !in_q;
            } else if ch == target && !in_q {
                return true;
            }
        }
        false
    };

    for line in content.lines() {
        let trimmed = line.trim();
        let is_config_line = (trimmed.starts_with("source_dir") || trimmed.starts_with("dest_dir"))
            && trimmed.contains('=');

        let starts_dest_dirs = trimmed.starts_with("dest_dirs") && trimmed.contains('=');

        if starts_dest_dirs {
            // Check if array is multi-line (has opening bracket but no closing bracket outside quotes on this line)
            if has_bracket_outside_quotes(trimmed, '[') && !has_bracket_outside_quotes(trimmed, ']')
            {
                in_dest_dirs_array = true;
            }
        }

        if is_config_line || starts_dest_dirs || in_dest_dirs_array {
            let processed = escape_backslashes_in_quotes(line);
            result.push_str(&processed);
            result.push('\n');

            if in_dest_dirs_array && has_bracket_outside_quotes(trimmed, ']') {
                in_dest_dirs_array = false;
            }
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }
    result
}

fn escape_backslashes_in_quotes(line: &str) -> String {
    let mut result = String::with_capacity(line.len() * 2);
    let mut in_quotes = false;
    let mut is_start_of_quote = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            in_quotes = !in_quotes;
            is_start_of_quote = in_quotes;
            result.push('"');
        } else if c == '\\' && in_quotes {
            let mut count = 1;
            while chars.peek() == Some(&'\\') {
                count += 1;
                chars.next();
            }
            if is_start_of_quote && count == 2 {
                result.push_str(r"\\\\");
            } else if count == 1 {
                result.push_str(r"\\");
            } else {
                for _ in 0..count {
                    result.push('\\');
                }
            }
            is_start_of_quote = false;
        } else {
            if in_quotes {
                is_start_of_quote = false;
            }
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn test_target_sync_config_block_size_nonzero() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dest = temp.path().join("dest");

        let target_zero = TargetSyncConfig {
            source_dir: src.clone(),
            dest_dir: TargetDir::from(dest.clone()),
            block_size_bytes: 0,
            block_sync_threshold_bytes: 1024,
            verify_writes: true,
            verification_mode: VerificationMode::Full,
            debounce_seconds: 3,
            retry_interval_seconds: 10,
            propagate_deletions: true,
        };
        assert_eq!(target_zero.block_size_nonzero().get(), 64 * 1024);

        let target_custom = TargetSyncConfig {
            source_dir: src,
            dest_dir: TargetDir::from(dest),
            block_size_bytes: 128 * 1024,
            block_sync_threshold_bytes: 1024,
            verify_writes: true,
            verification_mode: VerificationMode::Full,
            debounce_seconds: 3,
            retry_interval_seconds: 10,
            propagate_deletions: true,
        };
        assert_eq!(target_custom.block_size_nonzero().get(), 128 * 1024);
    }

    #[test]
    fn test_config_validation_valid() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_nested_and_identical_paths() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let nested_dest = src.join("nested_dest");
        let outside_dest = temp.path().join("dest");
        let nested_src = outside_dest.join("nested_src");

        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&nested_dest).unwrap();
        std::fs::create_dir_all(&outside_dest).unwrap();
        std::fs::create_dir_all(&nested_src).unwrap();

        // 1. Identical paths
        let cfg_identical = Config::builder(src.clone()).dest_dir(src.clone()).build();
        assert!(
            cfg_identical.validate().is_err(),
            "Identical source and destination directory must fail validation"
        );

        // 2. Destination nested inside source
        let cfg_dest_in_src = Config::builder(src.clone()).dest_dir(nested_dest).build();
        assert!(
            cfg_dest_in_src.validate().is_err(),
            "Destination directory nested within source directory must fail validation"
        );

        // 3. Source nested inside destination
        let cfg_src_in_dest = Config::builder(nested_src).dest_dir(outside_dest).build();
        assert!(
            cfg_src_in_dest.validate().is_err(),
            "Source directory nested within destination directory must fail validation"
        );
    }

    #[test]
    fn test_config_validation_block_size_and_threshold_limits() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        // 1. block_size_bytes > 64MB rejected
        let cfg_oversized_block = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(65 * 1024 * 1024)
            .block_sync_threshold_bytes(65 * 1024 * 1024)
            .build();
        assert!(
            cfg_oversized_block.validate().is_err(),
            "block_size_bytes exceeding 64MB must fail validation"
        );

        // 2. block_size_bytes == 64MB accepted
        let cfg_boundary_block = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(64 * 1024 * 1024)
            .block_sync_threshold_bytes(64 * 1024 * 1024)
            .build();
        assert!(
            cfg_boundary_block.validate().is_ok(),
            "block_size_bytes at boundary 64MB must pass validation"
        );

        // 3. block_sync_threshold_bytes < block_size_bytes rejected
        let cfg_invalid_threshold = Config::builder(src)
            .dest_dir(dst)
            .block_size_bytes(1024 * 1024)
            .block_sync_threshold_bytes(512 * 1024)
            .build();
        assert!(
            cfg_invalid_threshold.validate().is_err(),
            "block_sync_threshold_bytes smaller than block_size_bytes must fail validation"
        );
    }

    #[test]
    fn test_preprocess_config_toml_escaped_unc_not_double_expanded() {
        let input = r#"
        source_dir = "\\\\server\\share\\data"
        dest_dirs = [
            "\\\\nas\\backup1\\sub",
            "\\\\nas\\backup2"
        ]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10485760
        block_size_bytes = 1048576
        verify_writes = true
    "#;

        let processed = preprocess_config_toml(input);
        let config: Config = toml::from_str(&processed).expect("Config TOML parsing must succeed");

        assert_eq!(
            config.source_dir(),
            Path::new(r"\\server\share\data"),
            "Pre-escaped UNC source path must not be doubled to 4 backslashes"
        );

        let dests = config.resolved_dest_dirs();
        assert_eq!(dests[0], PathBuf::from(r"\\nas\backup1\sub"));
        assert_eq!(dests[1], PathBuf::from(r"\\nas\backup2"));
    }

    #[test]
    fn test_config_validation_missing_source() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let config = Config::builder(temp.path().join("nonexistent"))
            .dest_dir(dest)
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        // Soft validation: missing source directory logs a warning but validation passes
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_zero_debounce() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config::builder(source)
            .dest_dir(temp.path().join("dest"))
            .debounce_seconds(0)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_retry_interval() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config::builder(source)
            .dest_dir(temp.path().join("dest"))
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(0)
            .build();
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
            config.source_dir().to_string_lossy(),
            r#"Y:\Mill Processing\COMMON\MAINTENANCE"#
        );
        assert_eq!(
            config.dest_dir().unwrap().to_string_lossy(),
            r#"Z:\Backup\Folder"#
        );
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

    #[test]
    fn test_config_resolved_dest_dirs() {
        let config = Config::builder("C:\\src")
            .dest_dir(PathBuf::from("D:\\dst1"))
            .dest_dirs(vec![PathBuf::from("D:\\dst1"), PathBuf::from("E:\\dst2")])
            .debounce_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(10)
            .block_size_bytes(4)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], PathBuf::from("D:\\dst1"));
        assert_eq!(resolved[1], PathBuf::from("E:\\dst2"));
    }

    #[test]
    fn test_preprocess_dest_dirs_backslashes() {
        let input = r#"
            source_dir = "C:\source"
            dest_dir = "D:\Backup"
            dest_dirs = ["Y:\Mill Processing\COMMON", "Z:\Archive\Folder"]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let config: Config = toml::from_str(&processed).unwrap();
        assert_eq!(config.source_dir().to_string_lossy(), r"C:\source");
        assert_eq!(config.dest_dir().unwrap().to_string_lossy(), r"D:\Backup");
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved[1].to_string_lossy(), r"Y:\Mill Processing\COMMON");
        assert_eq!(resolved[2].to_string_lossy(), r"Z:\Archive\Folder");
    }

    #[test]
    fn test_config_only_dest_dirs() {
        let input = r#"
            source_dir = "C:\source"
            dest_dirs = ["D:\Backup1", "E:\Backup2"]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let config: Config = toml::from_str(&processed).unwrap();
        assert_eq!(config.destinations().len(), 2);
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], PathBuf::from(r"D:\Backup1"));
        assert_eq!(resolved[1], PathBuf::from(r"E:\Backup2"));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_no_dests() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config::builder(source)
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_preprocess_unc_paths_preserved() {
        let input = r#"
            source_dir = "C:\source"
            dest_dirs = ["\\172.16.0.60\scada_data\Files", "\\172.16.0.130\Files"]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let config: Config = toml::from_str(&processed).unwrap();
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(
            resolved[0].to_string_lossy(),
            r"\\172.16.0.60\scada_data\Files"
        );
        assert_eq!(resolved[1].to_string_lossy(), r"\\172.16.0.130\Files");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_resolved_dest_dirs_normalizes_single_backslash_unc() {
        let config = Config::builder("C:\\src")
            .dest_dir(PathBuf::from(r"\172.16.0.60\scada_data"))
            .dest_dirs(vec![PathBuf::from(r"\172.16.0.130\Files")])
            .debounce_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(10)
            .block_size_bytes(4)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].to_string_lossy(), r"\\172.16.0.60\scada_data");
        assert_eq!(resolved[1].to_string_lossy(), r"\\172.16.0.130\Files");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_invalid_relative_dest() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config::builder(source)
            .dest_dir(PathBuf::from("relative/folder/path"))
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_normalize_path_filtering() {
        assert_eq!(
            normalize_path(Path::new("X:/folder/subfolder/")).to_string_lossy(),
            r"X:\folder\subfolder"
        );
        assert_eq!(
            normalize_path(Path::new("\"Z:\\data\\files\\\"")).to_string_lossy(),
            r"Z:\data\files"
        );
        assert_eq!(
            normalize_path(Path::new(r"\172.16.0.193\share\")).to_string_lossy(),
            r"\\172.16.0.193\share"
        );
        assert_eq!(normalize_path(Path::new("C:\\")).to_string_lossy(), r"C:\");
    }

    #[test]
    fn test_normalize_drive_root_without_backslash() {
        assert_eq!(normalize_path(Path::new("R:")).to_string_lossy(), r"R:\");
        assert_eq!(normalize_path(Path::new("R:\\")).to_string_lossy(), r"R:\");
        assert_eq!(normalize_path(Path::new("R:/")).to_string_lossy(), r"R:\");
    }

    #[test]
    fn test_config_validate_mapped_drive() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();

        let config = Config::builder(source)
            .dest_dir(PathBuf::from("X:/Control IT Data/Files/"))
            .dest_dirs(vec![PathBuf::from(r"Z:\Backup\OPC\")])
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();

        assert!(config.validate().is_ok());
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].to_string_lossy(), r"X:\Control IT Data\Files");
        assert_eq!(resolved[1].to_string_lossy(), r"Z:\Backup\OPC");
    }

    #[test]
    fn test_config_validation_invalid_source_relative() {
        let config = Config::builder("relative/path/source")
            .dest_dir(PathBuf::from(r"C:\Backup"))
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();

        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, SyncError::Validation(ref msg) if msg.contains("Invalid source path"))
        );
    }

    #[test]
    fn test_normalize_paths_source_and_dest() {
        let config = Config::builder("C:/Source/Folder/")
            .dest_dir("D:/Dest/Folder/")
            .dest_dirs(vec![PathBuf::from("E:/Backup/Folder/")])
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();

        assert_eq!(config.source_dir().to_string_lossy(), r"C:\Source\Folder");
        let dests = config.resolved_dest_dirs();
        assert_eq!(dests[0].to_string_lossy(), r"D:\Dest\Folder");
        assert_eq!(dests[1].to_string_lossy(), r"E:\Backup\Folder");
    }

    #[test]
    fn test_resolved_dest_dirs_case_insensitive_dedup() {
        let config = Config::builder(r"C:\Source")
            .dest_dir(PathBuf::from(r"Z:\Backup\OPC"))
            .dest_dirs(vec![
                PathBuf::from(r"z:\backup\opc"),
                PathBuf::from(r"Z:\BACKUP\OPC\"),
                PathBuf::from(r"Y:\Different\Backup"),
            ])
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();

        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].to_string_lossy(), r"Z:\Backup\OPC");
        assert_eq!(resolved[1].to_string_lossy(), r"Y:\Different\Backup");
    }

    #[test]
    fn test_resolved_source_dir_alternate_resolution() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        std::fs::create_dir(&source_path).unwrap();

        let config = Config::test_default(source_path.clone(), temp.path().join("dest"));
        assert_eq!(config.resolved_source_dir(), &source_path);
    }

    #[test]
    fn test_preprocess_dest_dirs_multiline_array() {
        let input = r#"
            source_dir = "C:\source"
            dest_dirs = [
                "Y:\Mill Processing\COMMON",
                "Z:\Archive\Folder",
                "X:\Backup\Files",
            ]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let config: Config = toml::from_str(&processed).unwrap();
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].to_string_lossy(), r"Y:\Mill Processing\COMMON");
        assert_eq!(resolved[1].to_string_lossy(), r"Z:\Archive\Folder");
        assert_eq!(resolved[2].to_string_lossy(), r"X:\Backup\Files");
    }

    #[test]
    fn test_preprocess_dest_dirs_mixed_quotes_and_commas() {
        let input = r#"
            source_dir = "C:/source"
            dest_dirs = [
                'Y:/backup_folder_1',
                "Z:\backup_folder_2",
                "X:/backup_folder_3",
            ]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let config: Config = toml::from_str(&processed).unwrap();
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].to_string_lossy(), r"Y:\backup_folder_1");
        assert_eq!(resolved[1].to_string_lossy(), r"Z:\backup_folder_2");
        assert_eq!(resolved[2].to_string_lossy(), r"X:\backup_folder_3");
    }

    #[test]
    fn test_config_load_invalid_missing_comma_in_dest_dirs() {
        let input = r#"
            source_dir = "C:\source"
            dest_dirs = [
                "Y:\backup_folder_1"
                "X:\backup_folder_2"
            ]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let res: Result<Config, _> = toml::from_str(&processed);
        assert!(
            res.is_err(),
            "Missing comma in dest_dirs array must return syntax error"
        );
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("comma")
                || err_msg.contains("expected")
                || err_msg.contains("invalid"),
            "Error message should mention parsing failure: {}",
            err_msg
        );
    }

    #[test]
    fn test_validate_rejects_zero_block_size() {
        let mut config =
            Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"C:\dest"));
        config.block_size_bytes = 0;
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("block_size_bytes"));
    }

    #[test]
    fn test_validate_rejects_zero_threshold() {
        let mut config =
            Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"C:\dest"));
        config.block_sync_threshold_bytes = 0;
        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("block_sync_threshold_bytes")
        );
    }

    #[test]
    fn test_system_root() {
        let root = system_root();
        assert!(!root.as_os_str().is_empty());
    }

    #[test]
    fn test_builder_add_dest_dir_and_try_build() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst1 = temp.path().join("dst1");
        let dst2 = temp.path().join("dst2");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst1).unwrap();
        std::fs::create_dir_all(&dst2).unwrap();

        let config = Config::builder(&src)
            .dest_dir(&dst1)
            .add_dest_dir(&dst2)
            .try_build()
            .unwrap();

        assert_eq!(config.resolved_dest_dirs().len(), 2);
    }

    #[test]
    fn test_target_sync_config_from_config() {
        let config = Config::builder(r"C:\src")
            .dest_dir(r"C:\dst")
            .block_size_bytes(1024)
            .block_sync_threshold_bytes(2048)
            .verify_writes(true)
            .debounce_seconds(5)
            .retry_interval_seconds(15)
            .propagate_deletions(false)
            .build();
        let target = TargetSyncConfig::from_config(&config, PathBuf::from(r"C:\dst2"));
        assert_eq!(target.source_dir(), Path::new(r"C:\src"));
        assert_eq!(target.dest_dir(), Path::new(r"C:\dst2"));
        assert_eq!(target.block_size_bytes(), 1024);
        assert_eq!(target.block_sync_threshold_bytes(), 2048);
        assert!(target.verify_writes());
        assert_eq!(target.debounce_seconds(), 5);
        assert_eq!(target.retry_interval_seconds(), 15);
        assert!(!target.propagate_deletions());
    }

    #[test]
    fn test_target_dir_normalization_and_validation() {
        let t1 = TargetDir::new("X:/folder/subfolder/");
        assert_eq!(t1.as_path().to_string_lossy(), r"X:\folder\subfolder");
        assert!(t1.validate(TargetRole::Source).is_ok());

        let t2 = TargetDir::new("\"Z:\\data\\files\\\"");
        assert_eq!(t2.as_path().to_string_lossy(), r"Z:\data\files");
        assert!(t2.validate(TargetRole::Destination).is_ok());

        let t3 = TargetDir::new(r"\172.16.0.193\share\");
        assert_eq!(t3.as_path().to_string_lossy(), r"\\172.16.0.193\share");
        assert!(t3.validate(TargetRole::Source).is_ok());

        let t4 = TargetDir::new("R:");
        assert_eq!(t4.as_path().to_string_lossy(), r"R:\");
        assert!(t4.validate(TargetRole::Destination).is_ok());

        let rel = TargetDir::new("relative/source");
        assert!(rel.validate(TargetRole::Source).is_err());
        let err_msg = rel.validate(TargetRole::Source).unwrap_err().to_string();
        assert!(err_msg.contains("Invalid source path"));
    }

    #[test]
    fn test_destination_collection_dedup_and_order() {
        let col = DestinationCollection::from_raw(
            Some(PathBuf::from(r"D:\Backup1")),
            Some(vec![
                PathBuf::from(r"d:\backup1"), // duplicate, different case
                PathBuf::from(r"E:\Backup2"),
                PathBuf::from(r"\\172.16.0.60\scada_data"),
            ]),
        );
        assert_eq!(col.len(), 3);
        assert_eq!(col[0], TargetDir::new(r"D:\Backup1"));
        assert_eq!(col.get(1).unwrap(), &TargetDir::new(r"E:\Backup2"));
        assert_eq!(col[2], TargetDir::new(r"\\172.16.0.60\scada_data"));
        let paths = col.to_path_bufs();
        assert_eq!(paths[0], PathBuf::from(r"D:\Backup1"));
        assert_eq!(paths[1], PathBuf::from(r"E:\Backup2"));
        assert_eq!(paths[2], PathBuf::from(r"\\172.16.0.60\scada_data"));
    }

    #[test]
    fn test_config_destinations_matches_dest_dirs() {
        let d1 = PathBuf::from(r"D:\Backup1");
        let d2 = PathBuf::from(r"E:\Backup2");
        let config = Config::builder(r"C:\Source")
            .dest_dir(d1.clone())
            .add_dest_dir(d2.clone())
            .build();
        let slice: &[_] = config.destinations();
        assert_eq!(slice.len(), 2);
        let legacy = config.dest_dirs().unwrap();
        assert_eq!(slice[0].as_path(), legacy[0]);
        assert_eq!(slice[1].as_path(), legacy[1]);
    }

    #[test]
    fn test_target_sync_config_builder() {
        let builder = TargetSyncConfig::builder(r"C:\source", r"D:\dest")
            .block_size_bytes(512 * 1024)
            .block_sync_threshold_bytes(2 * 1024 * 1024)
            .verify_writes(false)
            .debounce_seconds(5)
            .retry_interval_seconds(20)
            .propagate_deletions(false);

        let cfg = builder.build().expect("valid builder should build");
        assert_eq!(cfg.source_dir(), Path::new(r"C:\source"));
        assert_eq!(cfg.dest_dir(), Path::new(r"D:\dest"));
        assert_eq!(cfg.block_size_bytes(), 512 * 1024);
        assert_eq!(cfg.block_sync_threshold_bytes(), 2 * 1024 * 1024);
        assert!(!cfg.verify_writes());
        assert_eq!(cfg.debounce_seconds(), 5);
        assert_eq!(cfg.retry_interval_seconds(), 20);
        assert!(!cfg.propagate_deletions());

        // Zero block size fails
        let invalid = TargetSyncConfig::builder(r"C:\source", r"D:\dest")
            .block_size_bytes(0)
            .build();
        assert!(invalid.is_err());

        // Block size > 64MB fails
        let invalid = TargetSyncConfig::builder(r"C:\source", r"D:\dest")
            .block_size_bytes(65 * 1024 * 1024)
            .build();
        assert!(invalid.is_err());

        // Zero threshold fails
        let invalid = TargetSyncConfig::builder(r"C:\source", r"D:\dest")
            .block_sync_threshold_bytes(0)
            .build();
        assert!(invalid.is_err());
    }

    #[test]
    fn test_preprocess_config_toml_with_bracketed_path_name() {
        let input = r#"
            source_dir = "C:\source"
            dest_dirs = [
                "D:\Backup[1]\Data",
                "E:\Backup[2]\Data"
            ]
            debounce_seconds = 3
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = true
        "#;
        let processed = preprocess_config_toml(input);
        let config: Config =
            toml::from_str(&processed).expect("should parse bracketed paths in array");
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].to_string_lossy(), r"D:\Backup[1]\Data");
        assert_eq!(resolved[1].to_string_lossy(), r"E:\Backup[2]\Data");
    }

    #[test]
    fn test_verification_mode_config_resolution() {
        // Legacy verify_writes = true resolves to Full
        let cfg1 = Config::builder(r"C:\source")
            .dest_dir(r"D:\dest")
            .verify_writes(true)
            .build();
        assert_eq!(cfg1.verification_mode(), VerificationMode::Full);

        // Legacy verify_writes = false resolves to Disabled
        let cfg2 = Config::builder(r"C:\source")
            .dest_dir(r"D:\dest")
            .verify_writes(false)
            .build();
        assert_eq!(cfg2.verification_mode(), VerificationMode::Disabled);

        // Explicit verification_mode overrides verify_writes
        let cfg3 = Config::builder(r"C:\source")
            .dest_dir(r"D:\dest")
            .verify_writes(true)
            .verification_mode(VerificationMode::MetadataAndFlush)
            .build();
        assert_eq!(cfg3.verification_mode(), VerificationMode::MetadataAndFlush);

        // TOML parsing with verification_mode
        let toml_str = r#"
            source_dir = "C:\\source"
            dest_dir = "D:\\dest"
            debounce_seconds = 1
            propagate_deletions = true
            block_sync_threshold_bytes = 10
            block_size_bytes = 4
            verify_writes = false
            verification_mode = "sampled"
        "#;
        let parsed: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.verification_mode(), VerificationMode::Sampled);
    }

    #[test]
    fn test_verification_mode_target_sync_config_plumbing() {
        let config = Config::builder(r"C:\source")
            .dest_dir(r"D:\dest")
            .verification_mode(VerificationMode::MetadataAndFlush)
            .build();
        let target = TargetSyncConfig::from_config(&config, r"D:\dest");
        assert_eq!(
            target.verification_mode(),
            VerificationMode::MetadataAndFlush
        );

        let target_builder = TargetSyncConfig::builder(r"C:\source", r"D:\dest")
            .verification_mode(VerificationMode::Sampled)
            .build()
            .unwrap();
        assert_eq!(
            target_builder.verification_mode(),
            VerificationMode::Sampled
        );
    }

    #[test]
    fn test_mutual_destination_overlap_rejected() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dest1 = temp.path().join("dest1");
        let dest2 = dest1.join("nested");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest2).unwrap();

        let config = Config::builder(&src).dest_dirs(vec![dest1, dest2]).build();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("nested within each other"));
    }

    #[test]
    fn test_target_sync_config_try_from() {
        let config_no_dest = Config::builder(r"C:\source").build();
        let res = TargetSyncConfig::try_from_config(&config_no_dest);
        assert!(res.is_err());

        let config_with_dest = Config::builder(r"C:\source").dest_dir(r"D:\dest").build();
        let res = TargetSyncConfig::try_from_config(&config_with_dest);
        assert!(res.is_ok());
    }

    #[test]
    fn test_target_sync_config_requires_explicit_construction() {
        // `From<&Config>` and `From<Config>` for TargetSyncConfig were intentionally removed
        // because they silently drop secondary destinations or fabricate empty targets.
        // Callers must use Config::target_configs() or TargetSyncConfig::try_from_config().
        let cfg = Config::test_default(r"C:\source", r"D:\dest");
        let targets = cfg.target_configs();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].dest_dir().to_string_lossy(), r"D:\dest");
    }
}

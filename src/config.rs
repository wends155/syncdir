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

/// Isolated target sync configuration for a specific destination directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSyncConfig {
    pub source_dir: PathBuf,
    pub dest_dir: PathBuf,
    pub block_size_bytes: u64,
    pub block_sync_threshold_bytes: u64,
    pub verify_writes: bool,
    pub debounce_seconds: u64,
    pub retry_interval_seconds: u64,
    pub propagate_deletions: bool,
}

impl TargetSyncConfig {
    /// Create a new `TargetSyncConfig` from a `Config` and a specific destination directory.
    pub fn from_config(config: &Config, dest_dir: PathBuf) -> Self {
        Self {
            source_dir: config.source_dir().to_path_buf(),
            dest_dir,
            block_size_bytes: config.block_size_bytes(),
            block_sync_threshold_bytes: config.block_sync_threshold_bytes(),
            verify_writes: config.verify_writes(),
            debounce_seconds: config.debounce_seconds(),
            retry_interval_seconds: config.retry_interval_seconds(),
            propagate_deletions: config.propagate_deletions(),
        }
    }

    /// Create a new `TargetSyncConfig`.
    #[deprecated(note = "Use TargetSyncConfig::from_config(config, dest) instead")]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_dir: PathBuf,
        dest_dir: PathBuf,
        block_size_bytes: u64,
        block_sync_threshold_bytes: u64,
        verify_writes: bool,
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
            debounce_seconds,
            retry_interval_seconds,
            propagate_deletions,
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

    /// Block size in bytes getter.
    pub fn block_size_bytes(&self) -> u64 {
        self.block_size_bytes
    }

    /// Block sync threshold in bytes getter.
    pub fn block_sync_threshold_bytes(&self) -> u64 {
        self.block_sync_threshold_bytes
    }

    /// Verify writes flag getter.
    pub fn verify_writes(&self) -> bool {
        self.verify_writes
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
}

impl From<&Config> for TargetSyncConfig {
    fn from(cfg: &Config) -> Self {
        let dest = cfg.dest_dir().map(Path::to_path_buf).unwrap_or_default();
        Self::from_config(cfg, dest)
    }
}

impl From<Config> for TargetSyncConfig {
    fn from(cfg: Config) -> Self {
        Self::from(&cfg)
    }
}

/// Strongly-typed, normalized synchronization root directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "PathBuf", into = "PathBuf")]
pub struct TargetDir(PathBuf);

impl TargetDir {
    /// Construct TargetDir by normalizing path via normalize_path(). Infallible.
    #[must_use]
    pub fn from_raw(path: impl Into<PathBuf>) -> Self {
        Self(normalize_path(&path.into()))
    }

    /// Validates path syntax for a given role ("source" or "destination").
    /// Accepts Windows drive letters (C:\), UNC prefixes (\\), and Unix absolute paths (/).
    pub fn validate(&self, role: &str) -> Result<(), SyncError> {
        let is_valid_drive_path = |path_str: &str| -> bool {
            if path_str.len() < 3 {
                return false;
            }
            let bytes = path_str.as_bytes();
            bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/')
        };
        let is_valid_unc_path = |path_str: &str| -> bool {
            path_str.starts_with(r"\\")
                && !path_str.starts_with(r"\\.\")
                && !path_str.starts_with(r"\\?\")
        };

        let s = self.0.to_string_lossy();
        let is_unc = is_valid_unc_path(&s);
        let is_drive = is_valid_drive_path(&s);
        let is_unix_abs = s.starts_with('/');

        if is_drive {
            tracing::debug!(target_path = %s, "Validated Windows drive path target");
        }

        if !is_unc && !is_drive && !is_unix_abs {
            let example_drive = if role == "source" {
                "C:\\, R:\\"
            } else {
                "C:\\, X:\\"
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
        Self::from_raw(p)
    }
}

impl From<&Path> for TargetDir {
    fn from(p: &Path) -> Self {
        Self::from_raw(p)
    }
}

impl From<&str> for TargetDir {
    fn from(s: &str) -> Self {
        Self::from_raw(s)
    }
}

impl From<TargetDir> for PathBuf {
    fn from(td: TargetDir) -> Self {
        td.0
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
            items.push(TargetDir::from_raw(p));
        }
        if let Some(adds) = additional {
            for a in adds {
                items.push(TargetDir::from_raw(a));
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
    #[serde(default = "default_retry_interval")]
    retry_interval_seconds: u64,
}

impl From<RawConfig> for Config {
    fn from(raw: RawConfig) -> Self {
        Self {
            source_dir: TargetDir::from_raw(raw.source_dir),
            destinations: DestinationCollection::from_raw(raw.dest_dir, raw.dest_dirs),
            debounce_seconds: raw.debounce_seconds,
            propagate_deletions: raw.propagate_deletions,
            block_sync_threshold_bytes: raw.block_sync_threshold_bytes,
            block_size_bytes: raw.block_size_bytes,
            verify_writes: raw.verify_writes,
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
    pub fn dest_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.dest_dirs = Some(dirs);
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
            source_dir: TargetDir::from_raw(self.source_dir),
            destinations: DestinationCollection::from_raw(self.dest_dir, self.dest_dirs),
            debounce_seconds: self.debounce_seconds,
            propagate_deletions: self.propagate_deletions,
            block_sync_threshold_bytes: self.block_sync_threshold_bytes,
            block_size_bytes: self.block_size_bytes,
            verify_writes: self.verify_writes,
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

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut s = path.to_string_lossy().trim().trim_matches('"').to_string();

    // Convert forward slashes to backslashes
    s = s.replace('/', "\\");

    // Ensure Windows drive letter root paths (e.g. "R:" or "X:") have a trailing backslash ("R:\")
    if s.len() == 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        s.push('\\');
    }

    // Repair single-backslash UNC network paths (\172... -> \\172...)
    if s.starts_with('\\') && !s.starts_with("\\\\") {
        let repaired = format!("\\{}", s);
        tracing::warn!(
            raw = %s,
            normalized = %repaired,
            "Normalized single-backslash path to UNC network path"
        );
        s = repaired;
    }

    // Trim redundant trailing backslashes while preserving root drive paths like C:\ or X:\
    while s.ends_with('\\') && s.len() > 3 {
        let is_root_drive =
            s.len() == 3 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic();
        if is_root_drive {
            break;
        }
        s.pop();
    }

    PathBuf::from(s)
}

pub use crate::net::{
    establish_smb_connection, find_mapped_drive_for_unc, resolve_mapped_drive_unc,
    try_resolve_alternate_path, try_resolve_unc_path,
};

impl Config {
    /// Mutate and normalize all path fields in-place.
    #[deprecated(note = "Paths are normalized automatically upon construction")]
    pub fn normalize_paths(&mut self) {}

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

    /// Return strongly-typed destination collection.
    pub fn destinations(&self) -> &DestinationCollection {
        &self.destinations
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

    /// Retry interval in seconds getter.
    pub fn retry_interval_seconds(&self) -> u64 {
        self.retry_interval_seconds
    }

    /// Resolve network paths (UNC shares / mapped drives) in-place for source and destinations.
    pub fn resolve_network_paths(&mut self) {
        self.source_dir =
            TargetDir::from_raw(try_resolve_alternate_path(self.source_dir.as_path()));
        self.destinations = DestinationCollection::new(
            self.destinations
                .iter()
                .map(|d| TargetDir::from_raw(try_resolve_alternate_path(d.as_path()))),
        );
    }

    /// Generate isolated target sync configurations for each configured destination directory.
    pub fn target_configs(&self) -> Vec<TargetSyncConfig> {
        self.resolved_dest_dirs()
            .into_iter()
            .map(|dest| TargetSyncConfig::from_config(self, dest))
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
        self.source_dir.validate("source")?;

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
            dest.validate("destination")?;
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
        if self.block_sync_threshold_bytes == 0 {
            return Err(SyncError::Validation(
                "block_sync_threshold_bytes must be greater than zero".into(),
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

    for line in content.lines() {
        let trimmed = line.trim();
        let is_config_line = (trimmed.starts_with("source_dir") || trimmed.starts_with("dest_dir"))
            && trimmed.contains('=');

        let starts_dest_dirs = trimmed.starts_with("dest_dirs") && trimmed.contains('=');

        if starts_dest_dirs {
            // Check if array is multi-line (has opening bracket but no closing bracket on this line)
            if trimmed.contains('[') && !trimmed.contains(']') {
                in_dest_dirs_array = true;
            }
        }

        if is_config_line || starts_dest_dirs || in_dest_dirs_array {
            let processed = escape_backslashes_in_quotes(line);
            result.push_str(&processed);
            result.push('\n');

            if in_dest_dirs_array && trimmed.contains(']') {
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
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            in_quotes = !in_quotes;
            result.push('"');
        } else if c == '\\' && in_quotes {
            if chars.peek() == Some(&'\\') {
                result.push('\\');
                result.push('\\');
                result.push('\\');
                result.push('\\');
                chars.next();
            } else {
                result.push('\\');
                result.push('\\');
            }
        } else {
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
    fn test_try_resolve_unc_path_unc_unchanged() {
        let unc_path = Path::new(r"\\172.16.0.60\share\folder");
        assert_eq!(try_resolve_unc_path(unc_path), unc_path);
    }

    #[test]
    fn test_try_resolve_unc_path_mapped_or_unmapped_drive() {
        let drive_path = Path::new(r"Z:\nonexistent_folder\subfolder");
        let resolved = try_resolve_unc_path(drive_path);
        if let Some(unc_base) = resolve_mapped_drive_unc("Z:") {
            let expected = format!(
                "{}\\{}",
                unc_base.trim_end_matches('\\'),
                r"nonexistent_folder\subfolder"
            );
            assert_eq!(resolved, PathBuf::from(expected));
        } else {
            assert_eq!(resolved, PathBuf::from(r"Z:\nonexistent_folder\subfolder"));
        }

        // Unmapped drive letter should return original normalized path
        let unmapped_path = Path::new(r"Q:\test_folder\subfolder");
        if resolve_mapped_drive_unc("Q:").is_none() {
            assert_eq!(
                try_resolve_unc_path(unmapped_path),
                PathBuf::from(r"Q:\test_folder\subfolder")
            );
        }
    }

    #[test]
    fn test_establish_smb_connection_non_unc() {
        // Non-UNC path should safely return Err without crashing
        assert!(establish_smb_connection(Path::new(r"C:\LocalFolder")).is_err());
    }

    #[test]
    fn test_find_mapped_drive_boundary_no_false_match() {
        // UNC path with non-existent host should safely return None
        assert!(find_mapped_drive_for_unc(Path::new(r"\\nonexistent_host_12345\share")).is_none());
    }

    #[test]
    fn test_try_resolve_alternate_path_local_unchanged() {
        let local_path = Path::new(r"C:\Users\CITECT\Documents");
        assert_eq!(
            try_resolve_alternate_path(local_path),
            normalize_path(local_path)
        );
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
    #[allow(deprecated)]
    fn test_normalize_paths_source_and_dest() {
        let mut config = Config::builder("C:/Source/Folder/")
            .dest_dir("D:/Dest/Folder/")
            .dest_dirs(vec![PathBuf::from("E:/Backup/Folder/")])
            .debounce_seconds(3)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .verify_writes(true)
            .retry_interval_seconds(10)
            .build();

        config.normalize_paths();
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
    #[allow(deprecated)]
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
        let mut config: Config = toml::from_str(&processed).unwrap();
        config.normalize_paths();
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
    #[allow(deprecated)]
    fn test_target_sync_config_new() {
        let target = TargetSyncConfig::new(
            PathBuf::from(r"C:\src"),
            PathBuf::from(r"C:\dst"),
            1024,
            2048,
            true,
            5,
            15,
            false,
        );
        assert_eq!(target.source_dir(), Path::new(r"C:\src"));
        assert_eq!(target.dest_dir(), Path::new(r"C:\dst"));
        assert_eq!(target.block_size_bytes(), 1024);
        assert_eq!(target.block_sync_threshold_bytes(), 2048);
        assert!(target.verify_writes());
        assert_eq!(target.debounce_seconds(), 5);
        assert_eq!(target.retry_interval_seconds(), 15);
        assert!(!target.propagate_deletions());
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
        let t1 = TargetDir::from_raw("X:/folder/subfolder/");
        assert_eq!(t1.as_path().to_string_lossy(), r"X:\folder\subfolder");
        assert!(t1.validate("source").is_ok());

        let t2 = TargetDir::from_raw("\"Z:\\data\\files\\\"");
        assert_eq!(t2.as_path().to_string_lossy(), r"Z:\data\files");
        assert!(t2.validate("destination").is_ok());

        let t3 = TargetDir::from_raw(r"\172.16.0.193\share\");
        assert_eq!(t3.as_path().to_string_lossy(), r"\\172.16.0.193\share");
        assert!(t3.validate("source").is_ok());

        let t4 = TargetDir::from_raw("R:");
        assert_eq!(t4.as_path().to_string_lossy(), r"R:\");
        assert!(t4.validate("destination").is_ok());

        let rel = TargetDir::from_raw("relative/source");
        assert!(rel.validate("source").is_err());
        let err_msg = rel.validate("source").unwrap_err().to_string();
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
        assert_eq!(col[0], TargetDir::from_raw(r"D:\Backup1"));
        assert_eq!(col.get(1).unwrap(), &TargetDir::from_raw(r"E:\Backup2"));
        assert_eq!(col[2], TargetDir::from_raw(r"\\172.16.0.60\scada_data"));
        let paths = col.to_path_bufs();
        assert_eq!(paths[0], PathBuf::from(r"D:\Backup1"));
        assert_eq!(paths[1], PathBuf::from(r"E:\Backup2"));
        assert_eq!(paths[2], PathBuf::from(r"\\172.16.0.60\scada_data"));
    }
}

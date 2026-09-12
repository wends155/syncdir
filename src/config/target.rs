//! Target and destination directory domain types for syncdir configuration.

use crate::error::SyncError;
use crate::path_util::normalize_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
#[serde(try_from = "RawTargetDir", into = "PathBuf")]
pub struct TargetDir(PathBuf);

#[derive(Deserialize)]
#[serde(transparent)]
struct RawTargetDir(PathBuf);

impl TryFrom<RawTargetDir> for TargetDir {
    type Error = SyncError;
    fn try_from(raw: RawTargetDir) -> Result<Self, Self::Error> {
        let normalized = normalize_path(raw.0);
        TargetDir::validate_internal(&normalized, None)?;
        Ok(TargetDir(normalized))
    }
}

impl TargetDir {
    /// Construct a validated TargetDir by normalizing path via normalize_path() and validating syntax for role.
    #[must_use]
    pub fn try_new(path: impl Into<PathBuf>, role: TargetRole) -> Result<Self, SyncError> {
        let dir = Self(normalize_path(path.into()));
        dir.validate(role)?;
        Ok(dir)
    }

    /// Construct TargetDir by normalizing path via normalize_path(). Infallible.
    #[deprecated(
        since = "0.2.0",
        note = "use TargetDir::try_new to enforce target syntax validation"
    )]
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(normalize_path(path.into()))
    }

    /// Construct TargetDir from an already validated path without deprecation warning.
    #[must_use]
    pub fn from_validated(path: impl Into<PathBuf>) -> Self {
        Self(normalize_path(path.into()))
    }

    /// Validates path syntax internally for an optional role.
    pub(crate) fn validate_internal(
        path: &Path,
        role: Option<TargetRole>,
    ) -> Result<(), SyncError> {
        let is_valid_drive_path = |path_str: &str| -> bool {
            if path_str.len() < 3 {
                return false;
            }
            let bytes = path_str.as_bytes();
            bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/')
        };

        let s = path.to_string_lossy();
        let is_unc = crate::path_util::parse_unc_host_and_share(path).is_some()
            && !s.starts_with(r"\\.\")
            && !s.starts_with(r"\\?\");
        let is_drive = is_valid_drive_path(&s);

        if !is_unc && !is_drive {
            let role_name = match role {
                Some(TargetRole::Source) => "source ",
                Some(TargetRole::Destination) => "destination ",
                None => "",
            };
            let example_drive = match role {
                Some(TargetRole::Source) => "C:\\, R:\\",
                Some(TargetRole::Destination) => "C:\\, X:\\",
                None => "C:\\, D:\\",
            };
            return Err(SyncError::validation_security(format!(
                "Invalid {role_name}path '{s}': must start with a drive letter (e.g. {example_drive}) or UNC network prefix (e.g. \\\\server\\share)"
            )));
        }
        Ok(())
    }

    /// Validates path syntax for a given role (`TargetRole::Source` or `TargetRole::Destination`).
    /// Accepts Windows drive letters (C:\) and UNC prefixes (\\).
    pub(crate) fn validate(&self, role: TargetRole) -> Result<(), SyncError> {
        Self::validate_internal(&self.0, Some(role))
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

impl TryFrom<PathBuf> for TargetDir {
    type Error = SyncError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        let normalized = normalize_path(path);
        Self::validate_internal(&normalized, None)?;
        Ok(Self(normalized))
    }
}

impl TryFrom<&Path> for TargetDir {
    type Error = SyncError;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let normalized = normalize_path(path.to_path_buf());
        Self::validate_internal(&normalized, None)?;
        Ok(Self(normalized))
    }
}

impl TryFrom<&str> for TargetDir {
    type Error = SyncError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let normalized = normalize_path(PathBuf::from(s));
        Self::validate_internal(&normalized, None)?;
        Ok(Self(normalized))
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
            items.push(TargetDir::from_validated(p));
        }
        if let Some(adds) = additional {
            for a in adds {
                items.push(TargetDir::from_validated(a));
            }
        }
        Self::new(items)
    }

    #[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_dir_try_new_validation() {
        // Valid Windows drive paths
        assert!(TargetDir::try_new(r"C:\valid\source", TargetRole::Source).is_ok());
        assert!(TargetDir::try_new(r"D:\valid\dest", TargetRole::Destination).is_ok());

        // Valid UNC paths
        assert!(TargetDir::try_new(r"\\server\share\data", TargetRole::Source).is_ok());
        assert!(TargetDir::try_new(r"\\server\share\backup", TargetRole::Destination).is_ok());

        // Relative path must fail validation
        let err_rel = TargetDir::try_new("relative/path", TargetRole::Source).unwrap_err();
        assert!(matches!(
            err_rel,
            crate::error::SyncError::Validation {
                kind: crate::error::ValidationKind::Security,
                ..
            }
        ));

        // Invalid device syntax (\\.\ or \\?\) must fail validation
        let err_dev = TargetDir::try_new(r"\\.\pipe\foo", TargetRole::Destination).unwrap_err();
        assert!(matches!(
            err_dev,
            crate::error::SyncError::Validation {
                kind: crate::error::ValidationKind::Security,
                ..
            }
        ));
    }

    #[test]
    fn test_target_dir_validate_drive_path_no_side_effect_logs() {
        let target = TargetDir(std::path::PathBuf::from(r"C:\data\sync"));
        let (res, log_output) =
            crate::test_support::with_captured_tracing(|| target.validate(TargetRole::Source));
        assert!(res.is_ok());
        assert!(
            log_output.is_empty(),
            "Expected zero log records from TargetDir::validate, got: {log_output}"
        );
    }

    #[test]
    fn test_target_dir_try_from_pathbuf() {
        let drive_path = PathBuf::from(r"C:\valid\source\path");
        let target = TargetDir::try_from(drive_path).unwrap();
        assert_eq!(target.as_path(), Path::new(r"C:\valid\source\path"));

        let unc_path = PathBuf::from(r"\\server\share\data");
        let target_unc = TargetDir::try_from(unc_path).unwrap();
        assert_eq!(target_unc.as_path(), Path::new(r"\\server\share\data"));

        let rel_path = PathBuf::from("relative/source");
        assert!(TargetDir::try_from(rel_path).is_err());

        let empty_path = PathBuf::from("");
        assert!(TargetDir::try_from(empty_path).is_err());
    }

    #[test]
    fn test_target_dir_try_from_path_and_str() {
        let target_ref = TargetDir::try_from(Path::new(r"D:\data\dest")).unwrap();
        assert_eq!(target_ref.as_path(), Path::new(r"D:\data\dest"));

        let target_str = TargetDir::try_from(r"E:\archive\dir").unwrap();
        assert_eq!(target_str.as_path(), Path::new(r"E:\archive\dir"));

        assert!(TargetDir::try_from(Path::new("not/absolute")).is_err());
        assert!(TargetDir::try_from("just_a_folder").is_err());
        assert!(TargetDir::try_from(r"\\.\pipe\bad").is_err());
    }

    #[test]
    fn test_target_dir_try_from_syntax_rejection() {
        assert!(TargetDir::try_from("").is_err());
        assert!(TargetDir::try_from("relative/path").is_err());
        assert!(TargetDir::try_from(r"\no_drive_or_unc").is_err());
        assert!(TargetDir::try_from(r"\\.\pipe\device").is_err());
        assert!(TargetDir::try_from(r"\\?\C:\verbatim").is_err());

        let validated = TargetDir::from_validated(r"C:\valid\prechecked");
        assert_eq!(validated.as_path(), Path::new(r"C:\valid\prechecked"));
    }
}

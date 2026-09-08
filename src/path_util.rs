//! Foundational path canonicalization and normalization utilities.
//!
//! # Purpose
//! This module provides string-level path canonicalization without requiring filesystem
//! access or network availability. It acts as a leaf dependency for `config` and `net`,
//! severing circular dependencies between configuration management and network resolution.
//!
//! # Key Functions
//! - [`normalize_path`]: Canonicalizes slashes, trims redundant trailing delimiters,
//!   guarantees trailing backslashes on Windows drive roots (e.g. `R:` -> `R:\`), and
//!   repairs single-backslash UNC network prefixes.

use std::path::{Path, PathBuf};

/// Normalizes a path string for Windows compatibility and internal consistency.
///
/// Converts forward slashes to backslashes, strips surrounding quotes, ensures drive roots
/// (e.g., `"R:"`) have a trailing backslash (`"R:\\"`), repairs malformed single-backslash
/// UNC prefixes (e.g., `"\172.16.0.1\share"` -> `"\\172.16.0.1\share"`), and trims
/// redundant trailing backslashes while preserving root drive roots.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use syncdir::path_util::normalize_path;
///
/// assert_eq!(
///     normalize_path(Path::new("X:/folder/subfolder/")).to_string_lossy(),
///     r"X:\folder\subfolder"
/// );
/// assert_eq!(
///     normalize_path(Path::new("R:")).to_string_lossy(),
///     r"R:\"
/// );
/// ```
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

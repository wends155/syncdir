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

use std::path::{Component, Path, PathBuf};

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
pub fn normalize_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let mut s = path.to_string_lossy().trim().trim_matches('"').to_string();

    // Convert forward slashes to backslashes
    s = s.replace('/', "\\");

    // Preserve lone backslash root or double backslash
    if s == "\\" || s == "\\\\" {
        return PathBuf::from(s);
    }

    // Ensure Windows drive letter root paths (e.g. "R:" or "X:") have a trailing backslash ("R:\")
    if s.len() == 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        s.push('\\');
    }

    // Repair single-backslash UNC network paths (\172... -> \\172...)
    if s.starts_with('\\') && !s.starts_with("\\\\") && s.len() > 1 && s[1..].contains('\\') {
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

/// Parses a UNC path into host and share components.
///
/// Returns `Some((host, share))` if the path is a valid UNC path (`\\host\share`),
/// or `None` if the path does not have both a non-empty host and share name.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use syncdir::path_util::parse_unc_host_and_share;
///
/// assert_eq!(
///     parse_unc_host_and_share(Path::new(r"\\server\share\subfolder")),
///     Some(("server", "share"))
/// );
/// assert_eq!(parse_unc_host_and_share(Path::new(r"\\server")), None);
/// assert_eq!(parse_unc_host_and_share(Path::new(r"C:\folder")), None);
/// ```
#[must_use]
pub fn parse_unc_host_and_share(path: &Path) -> Option<(&str, &str)> {
    let s = path.to_str()?;
    let trimmed = s.strip_prefix(r"\\")?;
    let mut parts = trimmed.splitn(3, '\\');
    let host = parts.next()?.trim();
    let share = parts.next()?.trim();
    if host.is_empty() || share.is_empty() {
        None
    } else {
        Some((host, share))
    }
}

/// Collapses `.` (current dir) and `..` (parent dir) components lexically without touching the filesystem.
///
/// Returns a `Vec<Component>` where `.` components are omitted, and `..` components
/// cancel out preceding `Normal` components.
///
/// # Examples
///
/// ```
/// use std::path::{Component, Path};
/// use syncdir::path_util::collapse_components;
///
/// let components = collapse_components(Path::new(r"C:\a\b\..\c"));
/// let path: std::path::PathBuf = components.iter().collect();
/// assert_eq!(path, std::path::PathBuf::from(r"C:\a\c"));
/// ```
#[must_use]
pub fn collapse_components(path: &Path) -> Vec<Component<'_>> {
    let mut out = Vec::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = out.last() {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            Component::CurDir => {}
            _ => out.push(comp),
        }
    }
    out
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

    #[test]
    fn test_normalize_single_backslash() {
        assert_eq!(normalize_path(r"\").to_string_lossy(), r"\");
        assert_eq!(normalize_path(r"/").to_string_lossy(), r"\");
        assert_eq!(normalize_path(r"\\").to_string_lossy(), r"\\");
    }

    #[test]
    fn test_parse_unc_host_and_share() {
        assert_eq!(
            parse_unc_host_and_share(Path::new(r"\\server\share")),
            Some(("server", "share"))
        );
        assert_eq!(
            parse_unc_host_and_share(Path::new(r"\\192.168.1.1\data\folder")),
            Some(("192.168.1.1", "data"))
        );
        assert_eq!(parse_unc_host_and_share(Path::new(r"\\server")), None);
        assert_eq!(parse_unc_host_and_share(Path::new(r"\\server\")), None);
        assert_eq!(parse_unc_host_and_share(Path::new(r"\\ \share")), None);
        assert_eq!(parse_unc_host_and_share(Path::new(r"C:\path")), None);
    }

    #[test]
    fn test_collapse_components() {
        let comps = collapse_components(Path::new(r"C:\a\b\..\c"));
        let p: PathBuf = comps.iter().collect();
        assert_eq!(p, PathBuf::from(r"C:\a\c"));

        let comps = collapse_components(Path::new(r"C:\a\.\b"));
        let p: PathBuf = comps.iter().collect();
        assert_eq!(p, PathBuf::from(r"C:\a\b"));

        let comps = collapse_components(Path::new(r"C:\..\a"));
        let p: PathBuf = comps.iter().collect();
        assert_eq!(p, PathBuf::from(r"C:\..\a"));
    }
}

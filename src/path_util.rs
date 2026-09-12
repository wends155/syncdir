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

use std::borrow::Cow;
use std::fmt;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SyncError;

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
        s = format!("\\{}", s);
    }

    // Trim redundant trailing backslashes while preserving root drive paths like C:\ or X:\ or UNC root \\ or \
    while s.ends_with('\\') {
        let is_root_drive =
            s.len() == 3 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic();
        if is_root_drive || s == "\\" || s == "\\\\" {
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

/// Check if `target` is the same directory as `base` or a nested descendant of `base`.
///
/// Uses Windows case-insensitive component comparison with lexical component collapsing.
#[must_use]
pub fn is_same_or_descendant(base: &Path, target: &Path) -> bool {
    let base_comps = collapse_components(base);
    let target_comps = collapse_components(target);
    if base_comps.is_empty() || target_comps.is_empty() || target_comps.len() < base_comps.len() {
        return false;
    }
    base_comps.iter().zip(target_comps.iter()).all(|(b, t)| {
        b.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&t.as_os_str().to_string_lossy())
    })
}

/// Returns the Windows system root directory (e.g. `C:\Windows`).
/// Reads `%SystemRoot%`, then `%windir%`, defaulting to `C:\Windows`.
#[must_use]
pub fn system_root() -> PathBuf {
    std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
}

/// Launches the system file explorer targeting the specified path.
///
/// Returns `Err(std::io::Error)` with `ErrorKind::NotFound` if the path does not exist
/// or if explorer is not found.
#[deprecated(since = "0.1.13", note = "Use tray::open_path instead")]
pub fn open_path(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Path does not exist: {}", path.display()),
        ));
    }
    #[cfg(target_os = "windows")]
    {
        let explorer = system_root().join("explorer.exe");
        if !explorer.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Explorer executable not found at {}", explorer.display()),
            ));
        }
        std::process::Command::new(explorer).arg(path).spawn()?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener).arg(path).spawn()?;
    }
    Ok(())
}

pub(crate) fn normalize_superscripts_cow<'a>(s: &'a str) -> Cow<'a, str> {
    if !s.bytes().any(|b| b >= 0x80) {
        return Cow::Borrowed(s);
    }
    if !s
        .chars()
        .any(|c| matches!(c, '⁰' | '¹' | '²' | '³' | '⁴'..='⁹'))
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '⁰' => out.push('0'),
            '¹' => out.push('1'),
            '²' => out.push('2'),
            '³' => out.push('3'),
            '⁴' => out.push('4'),
            '⁵' => out.push('5'),
            '⁶' => out.push('6'),
            '⁷' => out.push('7'),
            '⁸' => out.push('8'),
            '⁹' => out.push('9'),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

#[doc(hidden)]
pub fn is_safe_relative_path(path: &Path) -> bool {
    if !path.is_relative() || path.as_os_str().is_empty() {
        return false;
    }
    let s_raw = path.to_string_lossy();
    for seg in s_raw.split(['/', '\\']) {
        if seg == "." || seg == ".." {
            return false;
        }
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
        "CONOUT$", "CLOCK$",
    ];
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            Component::Normal(os_str) => {
                let s = os_str.to_string_lossy();
                if s.contains(':') {
                    return false;
                }
                // Reject Win32 wildcards and forbidden characters (including ASCII control characters)
                if s.chars()
                    .any(|c| matches!(c, '*' | '?' | '<' | '>' | '|' | '"') || (c as u32) < 0x20)
                {
                    return false;
                }
                // Reject components with trailing spaces or dots (Windows strips these)
                if s.ends_with(' ') || s.ends_with('.') {
                    return false;
                }
                // Normalize Unicode superscripts ('⁰'..'⁹') before stem extraction with zero-allocation on ASCII
                let normalized = normalize_superscripts_cow(&s);
                // Trim trailing spaces and dots before reserved name check
                let trimmed = normalized.trim_end_matches([' ', '.']);
                let stem = trimmed.split('.').next().unwrap_or("");
                if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Strongly-typed relative path domain newtype.
///
/// Guarantees:
/// - Path is relative (not empty, no drive letters, no root slashes, no UNC prefixes).
/// - Path has no directory traversal components (`..`).
/// - Path does not contain DOS device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`..`COM9`, `LPT1`..`LPT9`, etc.).
/// - Path does not contain Alternate Data Stream delimiters (`:`).
/// - Path does not contain Win32 forbidden characters (`*`, `?`, `<`, `>`, `|`, `"`).
/// - Path components do not have trailing spaces or dots.
/// - Slashes are normalized to forward slashes (`/`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct RelativePath(PathBuf);

impl RelativePath {
    /// Construct a new `RelativePath`, validating security invariants and normalizing slashes to `/`.
    /// Canonical constructor method.
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if the path is empty, absolute, contains directory traversals,
    /// DOS device names, Alternate Data Streams, or other unsafe patterns.
    pub fn try_new(path: impl AsRef<Path>) -> Result<Self, SyncError> {
        let p = path.as_ref();
        if p.as_os_str().is_empty() || p.to_string_lossy().trim().is_empty() {
            return Err(SyncError::validation_security(
                "Relative path cannot be empty",
            ));
        }

        let s = p.to_string_lossy();
        for seg in s.split(['/', '\\']) {
            if seg == "." || seg == ".." {
                return Err(SyncError::validation_security(format!(
                    "Invalid relative path containing '.' or '..' segment: {:?}",
                    p
                )));
            }
        }

        let normalized_str = s.replace('\\', "/");
        let normalized_path = PathBuf::from(normalized_str);

        if !is_safe_relative_path(&normalized_path) {
            return Err(SyncError::validation_security(format!(
                "Invalid or unsafe relative path: {:?}",
                p
            )));
        }

        Ok(Self(normalized_path))
    }

    /// Construct a new `RelativePath`, validating security invariants and normalizing slashes to `/`.
    /// Inline wrapper around `RelativePath::try_new`.
    #[inline]
    pub fn new(path: impl AsRef<Path>) -> Result<Self, SyncError> {
        Self::try_new(path)
    }

    /// Construct a `RelativePath` without running invariant checks.
    ///
    /// # Safety / Invariants
    /// Caller must guarantee that `path` has already been sanitized and uses forward slashes.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn from_sanitized_unchecked(path: PathBuf) -> Self {
        Self(path)
    }

    /// Borrows the underlying path slice.
    #[inline]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Converts to SQLite storage key format.
    #[inline]
    pub fn to_sqlite_key(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    /// Returns a new `RelativePath` with ASCII characters converted to lowercase.
    #[inline]
    pub fn to_ascii_lowercase(&self) -> Self {
        Self(PathBuf::from(self.0.to_string_lossy().to_ascii_lowercase()))
    }
}

impl Deref for RelativePath {
    type Target = Path;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for RelativePath {
    #[inline]
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::borrow::Borrow<Path> for RelativePath {
    #[inline]
    fn borrow(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

impl fmt::Debug for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RelativePath").field(&self.0).finish()
    }
}

impl From<RelativePath> for PathBuf {
    #[inline]
    fn from(rp: RelativePath) -> Self {
        rp.0
    }
}

impl TryFrom<&Path> for RelativePath {
    type Error = SyncError;

    #[inline]
    fn try_from(p: &Path) -> Result<Self, Self::Error> {
        Self::new(p)
    }
}

impl TryFrom<PathBuf> for RelativePath {
    type Error = SyncError;

    #[inline]
    fn try_from(p: PathBuf) -> Result<Self, Self::Error> {
        Self::new(p)
    }
}

impl PartialEq<Path> for RelativePath {
    #[inline]
    fn eq(&self, other: &Path) -> bool {
        self.0 == other
    }
}

impl PartialEq<&Path> for RelativePath {
    #[inline]
    fn eq(&self, other: &&Path) -> bool {
        self.0 == *other
    }
}

impl PartialEq<str> for RelativePath {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.0 == Path::new(other)
    }
}

impl PartialEq<&str> for RelativePath {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.0 == Path::new(*other)
    }
}

impl PartialEq<RelativePath> for Path {
    #[inline]
    fn eq(&self, other: &RelativePath) -> bool {
        self == other.0.as_path()
    }
}

impl PartialEq<RelativePath> for &Path {
    #[inline]
    fn eq(&self, other: &RelativePath) -> bool {
        *self == other.0.as_path()
    }
}

impl PartialEq<RelativePath> for str {
    #[inline]
    fn eq(&self, other: &RelativePath) -> bool {
        Path::new(self) == other.0.as_path()
    }
}

impl PartialEq<RelativePath> for &str {
    #[inline]
    fn eq(&self, other: &RelativePath) -> bool {
        Path::new(*self) == other.0.as_path()
    }
}

impl PartialEq<PathBuf> for RelativePath {
    #[inline]
    fn eq(&self, other: &PathBuf) -> bool {
        self.0 == *other
    }
}

impl PartialEq<RelativePath> for PathBuf {
    #[inline]
    fn eq(&self, other: &RelativePath) -> bool {
        *self == other.0
    }
}

impl PartialEq<&PathBuf> for RelativePath {
    #[inline]
    fn eq(&self, other: &&PathBuf) -> bool {
        self.0 == **other
    }
}

impl PartialEq<RelativePath> for &PathBuf {
    #[inline]
    fn eq(&self, other: &RelativePath) -> bool {
        **self == other.0
    }
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

    #[test]
    #[allow(deprecated)]
    fn test_path_util_hierarchies_and_system_root() {
        assert!(is_same_or_descendant(
            Path::new(r"C:\Users\Documents"),
            Path::new(r"C:\Users\Documents\Sub")
        ));
        assert!(is_same_or_descendant(
            Path::new(r"C:\Users\Documents"),
            Path::new(r"c:\users\documents")
        ));
        assert!(!is_same_or_descendant(
            Path::new(r"C:\Users\Documents"),
            Path::new(r"C:\Users\Other")
        ));

        let sys_root = system_root();
        assert!(!sys_root.as_os_str().is_empty());

        let res = open_path(Path::new(r"C:\NonExistent_syncdir_dummy_path_12345"));
        assert!(res.is_err());
    }

    #[test]
    fn test_is_same_or_descendant_empty_base_returns_false() {
        assert!(!is_same_or_descendant(
            Path::new(""),
            Path::new(r"C:\Users")
        ));
        assert!(!is_same_or_descendant(
            Path::new(r"C:\Users"),
            Path::new("")
        ));
        assert!(!is_same_or_descendant(Path::new(""), Path::new("")));
    }

    #[test]
    fn test_normalize_path_short_paths_and_root_preservation() {
        assert_eq!(normalize_path(r"fo\").to_string_lossy(), "fo");
        assert_eq!(normalize_path(r"a\").to_string_lossy(), "a");
        assert_eq!(normalize_path(r"fo/").to_string_lossy(), "fo");
        assert_eq!(normalize_path(r"C:\").to_string_lossy(), r"C:\");
        assert_eq!(normalize_path(r"\\").to_string_lossy(), r"\\");
        assert_eq!(normalize_path(r"\").to_string_lossy(), r"\");
    }

    #[test]
    fn test_relative_path_valid_simple_nested_unicode() {
        let valid_paths = &[
            "file.txt",
            "README.md",
            "docs/spec.pdf",
            "nested/sub/folder/data.bin",
            "nested\\windows\\separator.log",
            "документ/отчет.txt",
            "日本語/ノート.md",
            "münchen/straße.txt",
            "emoji/🎉_party.dat",
        ];

        for path_str in valid_paths {
            let rel = RelativePath::new(*path_str).unwrap_or_else(|e| {
                panic!("Expected valid path for '{}', got error: {:?}", path_str, e)
            });
            let expected = path_str.replace('\\', "/");
            assert_eq!(rel.as_path(), Path::new(&expected));
        }
    }

    #[test]
    fn test_relative_path_rejects_empty_and_absolute() {
        let invalid_paths = &[
            "",
            "   ",
            "\t",
            "\n",
            "/",
            "\\",
            "//",
            "\\\\",
            "C:\\foo",
            "C:/foo",
            "c:\\nested\\file.txt",
            "/foo",
            "\\foo",
            "/nested/file.txt",
            "\\nested\\file.txt",
            "\\\\server\\share\\file.txt",
            "//server/share/file.txt",
        ];

        for path_str in invalid_paths {
            let res = RelativePath::new(*path_str);
            assert!(
                res.is_err(),
                "Expected '{}' to be rejected as empty or absolute, but got: {:?}",
                path_str,
                res.map(|r| r.as_path().to_path_buf())
            );
        }
    }

    #[test]
    fn test_relative_path_rejects_traversal_devices_trailing_ads() {
        let unsafe_paths = &[
            // Directory traversals
            "..",
            "../file.txt",
            "docs/../secret.txt",
            "nested/dir/..",
            ".",
            "./file.txt",
            "nested/./file.txt",
            // DOS devices
            "CON",
            "PRN",
            "AUX",
            "NUL",
            "COM1",
            "COM9",
            "LPT1",
            "LPT9",
            "CONIN$",
            "CONOUT$",
            "CLOCK$",
            "subdir/CON",
            "nested/NUL.txt",
            "dir/com1.dat",
            "lpt3.log",
            // Unicode superscripts
            "COM¹",
            "LPT²",
            // Trailing spaces & dots
            "file.txt ",
            "file.txt.",
            "dir /file.txt",
            "dir./file.txt",
            "CON ",
            "NUL.",
            // Alternate Data Streams
            "file.txt:stream",
            "dir:ads/file.txt",
            "foo:$DATA",
            // Win32 forbidden characters
            "file*.txt",
            "file?.bin",
            "<tag>.dat",
            "pipe|name",
            "quote\"name",
        ];

        for path_str in unsafe_paths {
            let res = RelativePath::new(*path_str);
            assert!(
                res.is_err(),
                "Expected '{}' to be rejected for safety/traversal/device/ads, but got Ok",
                path_str
            );
        }
    }

    #[test]
    fn test_relative_path_traits_and_methods() {
        let rel = RelativePath::new("docs\\nested\\spec.txt").unwrap();

        // Path normalization
        assert_eq!(rel.as_path(), Path::new("docs/nested/spec.txt"));

        // Deref
        assert_eq!(rel.file_name(), Some(std::ffi::OsStr::new("spec.txt")));

        // AsRef
        fn assert_as_ref(p: impl AsRef<Path>) {
            assert_eq!(p.as_ref(), Path::new("docs/nested/spec.txt"));
        }
        assert_as_ref(&rel);

        // Borrow & HashMap
        let mut map = std::collections::HashMap::new();
        map.insert(rel.clone(), 42);
        assert_eq!(map.get(Path::new("docs/nested/spec.txt")), Some(&42));

        // Display & Debug
        assert_eq!(format!("{}", rel), "docs/nested/spec.txt");
        assert_eq!(
            format!("{:?}", rel),
            "RelativePath(\"docs/nested/spec.txt\")"
        );

        // to_sqlite_key
        assert_eq!(rel.to_sqlite_key(), "docs/nested/spec.txt");

        // to_ascii_lowercase
        let upper_rel = RelativePath::new("Docs/Nested/SPEC.txt").unwrap();
        assert_eq!(
            upper_rel.to_ascii_lowercase().as_path(),
            Path::new("docs/nested/spec.txt")
        );

        // Conversions
        let pb: PathBuf = rel.clone().into();
        assert_eq!(pb, PathBuf::from("docs/nested/spec.txt"));

        let try_from_ref = RelativePath::try_from(Path::new("a/b.txt")).unwrap();
        assert_eq!(try_from_ref.as_path(), Path::new("a/b.txt"));

        let try_from_buf = RelativePath::try_from(PathBuf::from("a/b.txt")).unwrap();
        assert_eq!(try_from_buf.as_path(), Path::new("a/b.txt"));

        // Cross-type PartialEq
        assert_eq!(rel, *Path::new("docs/nested/spec.txt"));
        assert_eq!(rel, Path::new("docs/nested/spec.txt"));
        assert_eq!(rel, "docs/nested/spec.txt");
        assert_eq!(rel, *"docs/nested/spec.txt");
        assert_eq!(*Path::new("docs/nested/spec.txt"), rel);
        assert_eq!(Path::new("docs/nested/spec.txt"), rel);
        assert_eq!("docs/nested/spec.txt", rel);
        assert_eq!(*"docs/nested/spec.txt", rel);

        // Serde roundtrip via toml
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
        struct Wrapper {
            path: RelativePath,
        }
        let wrapper = Wrapper { path: rel.clone() };
        let toml_str = toml::to_string(&wrapper).unwrap();
        let decoded: Wrapper = toml::from_str(&toml_str).unwrap();
        assert_eq!(decoded, wrapper);

        // Serde deserialization invariant check
        let invalid_toml = "path = \"../traversal.txt\"\n";
        let err = toml::from_str::<Wrapper>(invalid_toml);
        assert!(err.is_err());
    }

    #[test]
    fn test_normalize_path_single_backslash_unc_no_side_effect_logs() {
        let (norm, log_output) = crate::test_support::with_captured_tracing(|| {
            normalize_path(r"\172.16.0.1\share\sub\file.txt")
        });
        assert_eq!(norm, PathBuf::from(r"\\172.16.0.1\share\sub\file.txt"));
        assert!(
            log_output.is_empty(),
            "Expected zero log records from normalize_path, got: {log_output}"
        );
    }

    #[test]
    fn test_relative_path_bidirectional_partial_eq_with_pathbuf() {
        use std::path::{Path, PathBuf};
        let rel = RelativePath::try_new("docs/readme.md").unwrap();
        let pb = PathBuf::from("docs/readme.md");
        let p = Path::new("docs/readme.md");

        assert_eq!(rel, pb);
        assert_eq!(pb, rel);
        assert_eq!(rel, p);
        assert_eq!(p, rel);

        let diff = PathBuf::from("other/file.txt");
        assert_ne!(rel, diff);
        assert_ne!(diff, rel);
    }
}

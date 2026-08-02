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
    #[serde(default)]
    pub dest_dir: Option<PathBuf>,
    pub debounce_seconds: u64,
    pub propagate_deletions: bool,
    pub block_sync_threshold_bytes: u64,
    pub block_size_bytes: u64,
    pub verify_writes: bool,
    #[serde(default = "default_retry_interval")]
    pub retry_interval_seconds: u64,
    #[serde(default)]
    pub dest_dirs: Option<Vec<PathBuf>>,
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
        let is_root_drive = s.len() == 3 && s.as_bytes()[1] == b':';
        if is_root_drive {
            break;
        }
        s.pop();
    }

    PathBuf::from(s)
}

fn normalize_dest_path(path: &Path) -> PathBuf {
    normalize_path(path)
}

/// Query Windows Win32 API `WNetGetConnectionW` to resolve a local drive letter (e.g. "R:")
/// to its underlying remote UNC share path (e.g. "\\172.16.0.193\share").
/// Returns `None` on non-Windows platforms, unmapped drives, or API errors.
#[cfg(target_os = "windows")]
pub fn resolve_mapped_drive_unc(drive_prefix: &str) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let local_name: Vec<u16> = std::ffi::OsStr::new(drive_prefix)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = vec![0u16; 512];
    let mut len = buf.len() as u32;

    #[link(name = "mpr")]
    unsafe extern "system" {
        fn WNetGetConnectionW(
            lpLocalName: *const u16,
            lpRemoteName: *mut u16,
            lpnLength: *mut u32,
        ) -> u32;
    }

    let ret = unsafe { WNetGetConnectionW(local_name.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if ret == 0 {
        let unc_str = String::from_utf16_lossy(&buf[..len as usize])
            .trim_matches('\0')
            .to_string();
        if !unc_str.is_empty() {
            return Some(unc_str);
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
pub fn resolve_mapped_drive_unc(_drive_prefix: &str) -> Option<String> {
    None
}

/// Attempt to convert a path starting with a Windows drive letter into a full UNC network path.
/// If the path starts with a drive letter and `WNetGetConnectionW` succeeds, returns the combined UNC path.
/// Otherwise, returns the original normalized path unchanged.
pub fn try_resolve_unc_path(path: &Path) -> PathBuf {
    let normalized = normalize_path(path);
    let s = normalized.to_string_lossy();

    // Check if path starts with a drive letter e.g. "R:\" or "R:foo"
    if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        let drive_letter = &s[..2]; // e.g. "R:"
        if let Some(unc_base) = resolve_mapped_drive_unc(drive_letter) {
            let relative = s[2..].trim_start_matches('\\');
            if relative.is_empty() {
                return PathBuf::from(unc_base);
            } else {
                return PathBuf::from(format!("{}\\{}", unc_base.trim_end_matches('\\'), relative));
            }
        }
    }

    normalized
}

/// Establish or refresh a Win32 SMB network connection for a UNC path using `WNetAddConnection2W`.
/// Leverages stored credentials in Windows Credential Manager or session tokens.
/// Returns `true` if `WNetAddConnection2W` succeeded or connection already exists.
#[cfg(target_os = "windows")]
pub fn establish_smb_connection(unc_path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let s = unc_path.to_string_lossy();
    if !s.starts_with(r"\\") {
        return false;
    }

    // Extract root share e.g. "\\172.16.0.193\Files" or "\\172.16.0.193\ABB Industrial IT Data"
    let parts: Vec<&str> = s[2..].split('\\').collect();
    if parts.len() < 2 {
        return false;
    }
    let unc_share = format!(r"\\{}\{}", parts[0], parts[1]);

    let unc_share_w: Vec<u16> = std::ffi::OsStr::new(&unc_share)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    #[repr(C)]
    struct NETRESOURCEW {
        dwScope: u32,
        dwType: u32,
        dwDisplayType: u32,
        dwUsage: u32,
        lpLocalName: *const u16,
        lpRemoteName: *const u16,
        lpComment: *const u16,
        lpProvider: *const u16,
    }

    #[link(name = "mpr")]
    unsafe extern "system" {
        fn WNetAddConnection2W(
            lpNetResource: *const NETRESOURCEW,
            lpPassword: *const u16,
            lpUserName: *const u16,
            dwFlags: u32,
        ) -> u32;
    }

    let nr = NETRESOURCEW {
        dwScope: 0,
        dwType: 1, // RESOURCETYPE_DISK
        dwDisplayType: 0,
        dwUsage: 0,
        lpLocalName: std::ptr::null(),
        lpRemoteName: unc_share_w.as_ptr(),
        lpComment: std::ptr::null(),
        lpProvider: std::ptr::null(),
    };

    let ret = unsafe { WNetAddConnection2W(&nr, std::ptr::null(), std::ptr::null(), 0) };

    // 0 = NO_ERROR, 85 = ERROR_ALREADY_ASSIGNED, 1219 = ERROR_SESSION_CREDENTIAL_CONFLICT
    ret == 0 || ret == 85 || ret == 1219
}

#[cfg(not(target_os = "windows"))]
pub fn establish_smb_connection(_unc_path: &Path) -> bool {
    false
}

/// Reverse-lookup active Win32 mapped drive letters ('A'..='Z') to find a drive letter
/// mapped to a prefix of the given UNC path.
pub fn find_mapped_drive_for_unc(unc_path: &Path) -> Option<PathBuf> {
    let normalized = normalize_path(unc_path);
    let unc_str = normalized.to_string_lossy().to_lowercase();
    if !unc_str.starts_with(r"\\") {
        return None;
    }

    for letter in (b'A'..=b'Z').map(|b| b as char) {
        let drive_prefix = format!("{}:", letter);
        if let Some(mapped_unc) = resolve_mapped_drive_unc(&drive_prefix) {
            let mapped_lower = mapped_unc.trim_end_matches('\\').to_lowercase();
            if !mapped_lower.is_empty() && unc_str.starts_with(&mapped_lower) {
                let relative = unc_str[mapped_lower.len()..].trim_start_matches('\\');
                if relative.is_empty() {
                    return Some(PathBuf::from(format!(r"{}\", drive_prefix)));
                } else {
                    return Some(PathBuf::from(format!(r"{}\{}", drive_prefix, relative)));
                }
            }
        }
    }
    None
}

/// Attempt bidirectional resolution of a destination path:
///
/// 1. If path is a drive letter (e.g. `R:\...`), attempts `try_resolve_unc_path`.
/// 2. If path is a UNC share (e.g. `\\172.16.0.193\...`), attempts `establish_smb_connection`
///    and `find_mapped_drive_for_unc`.
///
/// Returns the resolved alternate path if accessible, or original normalized path.
pub fn try_resolve_alternate_path(path: &Path) -> PathBuf {
    let normalized = normalize_path(path);

    // If path is accessible directly, return normalized
    if matches!(std::fs::metadata(&normalized), Ok(m) if m.is_dir()) {
        return normalized;
    }

    let s = normalized.to_string_lossy();

    // Case 1: Drive letter path (e.g. "R:\...")
    if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        let unc_path = try_resolve_unc_path(&normalized);
        if unc_path != normalized {
            // Attempt establishing SMB session on resolved UNC share
            establish_smb_connection(&unc_path);
            if matches!(std::fs::metadata(&unc_path), Ok(m) if m.is_dir()) {
                return unc_path;
            }
        }
    }

    // Case 2: UNC path (e.g. "\\172.16.0.193\Files")
    if s.starts_with(r"\\") {
        // Attempt SMB session establishment on UNC path
        establish_smb_connection(&normalized);
        if matches!(std::fs::metadata(&normalized), Ok(m) if m.is_dir()) {
            return normalized;
        }

        // Try mapped drive reverse resolution
        if let Some(mapped_drive_path) = find_mapped_drive_for_unc(&normalized)
            && matches!(std::fs::metadata(&mapped_drive_path), Ok(m) if m.is_dir())
        {
            return mapped_drive_path;
        }
    }

    normalized
}

impl Config {
    /// Mutate and normalize all path fields (`source_dir`, `dest_dir`, `dest_dirs`) in-place.
    pub fn normalize_paths(&mut self) {
        self.source_dir = normalize_path(&self.source_dir);
        if let Some(ref mut d) = self.dest_dir {
            *d = normalize_path(d);
        }
        if let Some(ref mut dirs) = self.dest_dirs {
            for d in dirs.iter_mut() {
                *d = normalize_path(d);
            }
        }
    }

    /// Return the resolved, normalized source directory path, attempting `try_resolve_alternate_path`
    /// if the raw path is unreachable or on a mapped drive / network share.
    pub fn resolved_source_dir(&self) -> PathBuf {
        try_resolve_alternate_path(&self.source_dir)
    }

    /// Return a merged, deduplicated list of all configured destination directories.
    /// Includes `dest_dir` first (if set), then appends any unique entries from `dest_dirs`.
    /// Automatically normalizes single-backslash UNC network paths (`\172...` -> `\\172...`)
    /// and performs case-insensitive path deduplication on Windows.
    pub fn resolved_dest_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(ref primary) = self.dest_dir {
            let normalized = normalize_dest_path(primary);
            dirs.push(normalized);
        }
        if let Some(ref list) = self.dest_dirs {
            for d in list {
                let normalized = normalize_dest_path(d);
                let norm_lower = normalized.to_string_lossy().to_lowercase();
                if !dirs
                    .iter()
                    .any(|existing| existing.to_string_lossy().to_lowercase() == norm_lower)
                {
                    dirs.push(normalized);
                }
            }
        }
        dirs
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
        let mut config: Config =
            toml::from_str(&processed).map_err(|e| SyncError::Config(e.to_string()))?;
        config.normalize_paths();
        Ok(config)
    }

    /// Validate that configured directories exist and parameters are valid.
    /// Enforces that source and destination paths are valid drive paths (`C:\`) or UNC network paths (`\\`).
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if parameters are invalid.
    pub fn validate(&self) -> Result<(), SyncError> {
        let src_str = self.source_dir.to_string_lossy();
        let src_unc = src_str.starts_with("\\\\");
        let src_drive = src_str.len() >= 2
            && src_str.as_bytes()[1] == b':'
            && src_str.as_bytes()[0].is_ascii_alphabetic();
        let src_unix = src_str.starts_with('/');

        if !src_unc && !src_drive && !src_unix {
            return Err(SyncError::Validation(format!(
                "Invalid source path '{}': must start with a drive letter (e.g. C:\\, R:\\) or UNC network prefix (e.g. \\\\server\\share)",
                src_str
            )));
        }

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

        let dests = self.resolved_dest_dirs();
        if dests.is_empty() {
            return Err(SyncError::Validation(
                "At least one destination directory must be specified (via dest_dir or dest_dirs)"
                    .into(),
            ));
        }

        for dest in &dests {
            let s = dest.to_string_lossy();
            let is_unc = s.starts_with("\\\\");
            let is_drive =
                s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic();
            let is_unix_abs = s.starts_with('/');

            if is_drive {
                tracing::debug!(
                    target_path = %s,
                    "Validated Windows drive path target (local or mapped drive)"
                );
            }

            if !is_unc && !is_drive && !is_unix_abs {
                return Err(SyncError::Validation(format!(
                    "Invalid destination path '{}': must start with a drive letter (e.g. C:\\, X:\\) or UNC network prefix (e.g. \\\\server\\share)",
                    s
                )));
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

impl Config {
    /// Create a Config with sensible test defaults for the given source and dest.
    #[doc(hidden)]
    pub fn test_default(source: PathBuf, dest: PathBuf) -> Self {
        let mut cfg = Self {
            source_dir: source,
            dest_dir: Some(dest),
            debounce_seconds: 1,
            propagate_deletions: true,
            block_sync_threshold_bytes: 10,
            block_size_bytes: 4,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
        };
        cfg.normalize_paths();
        cfg
    }
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

        let config = Config {
            source_dir: source,
            dest_dir: Some(dest),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
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
            dest_dir: Some(dest),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
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
            dest_dir: Some(temp.path().join("dest")),
            debounce_seconds: 0,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
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
            dest_dir: Some(temp.path().join("dest")),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 0,
            dest_dirs: None,
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
        assert_eq!(
            config.dest_dir.unwrap().to_string_lossy(),
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
        let config = Config {
            source_dir: PathBuf::from("C:\\src"),
            dest_dir: Some(PathBuf::from("D:\\dst1")),
            dest_dirs: Some(vec![PathBuf::from("D:\\dst1"), PathBuf::from("E:\\dst2")]),
            debounce_seconds: 1,
            propagate_deletions: true,
            block_sync_threshold_bytes: 10,
            block_size_bytes: 4,
            verify_writes: true,
            retry_interval_seconds: 10,
        };
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
        assert_eq!(config.source_dir.to_string_lossy(), r"C:\source");
        assert_eq!(config.dest_dir.unwrap().to_string_lossy(), r"D:\Backup");
        let extras = config.dest_dirs.unwrap();
        assert_eq!(extras[0].to_string_lossy(), r"Y:\Mill Processing\COMMON");
        assert_eq!(extras[1].to_string_lossy(), r"Z:\Archive\Folder");
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
        assert!(config.dest_dir.is_none());
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

        let config = Config {
            source_dir: source,
            dest_dir: None,
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
        };
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
        let config = Config {
            source_dir: PathBuf::from("C:\\src"),
            dest_dir: Some(PathBuf::from(r"\172.16.0.60\scada_data")),
            dest_dirs: Some(vec![PathBuf::from(r"\172.16.0.130\Files")]),
            debounce_seconds: 1,
            propagate_deletions: true,
            block_sync_threshold_bytes: 10,
            block_size_bytes: 4,
            verify_writes: true,
            retry_interval_seconds: 10,
        };
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

        let config = Config {
            source_dir: source,
            dest_dir: Some(PathBuf::from("relative/folder/path")),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
        };
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
        // Non-UNC path should safely return false without crashing
        assert!(!establish_smb_connection(Path::new(r"C:\LocalFolder")));
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

        let config = Config {
            source_dir: source,
            dest_dir: Some(PathBuf::from("X:/Control IT Data/Files/")),
            dest_dirs: Some(vec![PathBuf::from(r"Z:\Backup\OPC\")]),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
        };

        assert!(config.validate().is_ok());
        let resolved = config.resolved_dest_dirs();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].to_string_lossy(), r"X:\Control IT Data\Files");
        assert_eq!(resolved[1].to_string_lossy(), r"Z:\Backup\OPC");
    }

    #[test]
    fn test_config_validation_invalid_source_relative() {
        let config = Config {
            source_dir: PathBuf::from("relative/path/source"),
            dest_dir: Some(PathBuf::from(r"C:\Backup")),
            dest_dirs: None,
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
        };

        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, SyncError::Validation(ref msg) if msg.contains("Invalid source path"))
        );
    }

    #[test]
    fn test_normalize_paths_source_and_dest() {
        let mut config = Config {
            source_dir: PathBuf::from("C:/Source/Folder/"),
            dest_dir: Some(PathBuf::from("D:/Dest/Folder/")),
            dest_dirs: Some(vec![PathBuf::from("E:/Backup/Folder/")]),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
        };

        config.normalize_paths();
        assert_eq!(config.source_dir.to_string_lossy(), r"C:\Source\Folder");
        assert_eq!(
            config.dest_dir.unwrap().to_string_lossy(),
            r"D:\Dest\Folder"
        );
        assert_eq!(
            config.dest_dirs.unwrap()[0].to_string_lossy(),
            r"E:\Backup\Folder"
        );
    }

    #[test]
    fn test_resolved_dest_dirs_case_insensitive_dedup() {
        let config = Config {
            source_dir: PathBuf::from(r"C:\Source"),
            dest_dir: Some(PathBuf::from(r"Z:\Backup\OPC")),
            dest_dirs: Some(vec![
                PathBuf::from(r"z:\backup\opc"),
                PathBuf::from(r"Z:\BACKUP\OPC\"),
                PathBuf::from(r"Y:\Different\Backup"),
            ]),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: 1024,
            block_size_bytes: 512,
            verify_writes: true,
            retry_interval_seconds: 10,
        };

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
        assert_eq!(config.resolved_source_dir(), source_path);
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
        let extras = config.dest_dirs.unwrap();
        assert_eq!(extras.len(), 3);
        assert_eq!(extras[0].to_string_lossy(), r"Y:\Mill Processing\COMMON");
        assert_eq!(extras[1].to_string_lossy(), r"Z:\Archive\Folder");
        assert_eq!(extras[2].to_string_lossy(), r"X:\Backup\Files");
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
}

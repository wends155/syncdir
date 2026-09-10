//! Network and Win32 UNC/SMB connection management.
//!
//! Provides utilities for querying mapped drive UNC targets, establishing SMB sessions,
//! and resolving alternate network paths.

use crate::error::SyncError;
use crate::path_util::normalize_path;
use std::path::{Path, PathBuf};

/// Network resolution abstraction for resolving alternate UNC paths and establishing SMB connections.
pub trait NetworkResolver: Send + Sync {
    /// Attempts to resolve an alternate path (e.g. drive letter to UNC path or vice versa).
    fn try_resolve_alternate_path(&self, path: &Path) -> PathBuf;

    /// Attempts to resolve a mapped drive path to its underlying UNC path.
    fn try_resolve_unc_path(&self, path: &Path) -> PathBuf;

    /// Attempts to establish an SMB connection to a UNC network share.
    ///
    /// # Errors
    /// Returns `SyncError` if the share is invalid or connection fails.
    fn establish_smb_connection(&self, unc_path: &Path) -> Result<(), SyncError>;

    /// Probes whether the destination root path is currently accessible.
    ///
    /// Default implementation checks if `path` exists and is a directory.
    fn is_destination_accessible(&self, path: &Path) -> bool {
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    }
}

/// Default Win32 production network resolver using OS APIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32NetworkResolver;

impl NetworkResolver for Win32NetworkResolver {
    fn try_resolve_alternate_path(&self, path: &Path) -> PathBuf {
        try_resolve_alternate_path(path)
    }

    fn try_resolve_unc_path(&self, path: &Path) -> PathBuf {
        try_resolve_unc_path(path)
    }

    fn establish_smb_connection(&self, unc_path: &Path) -> Result<(), SyncError> {
        establish_smb_connection(unc_path)
    }
}

#[derive(Debug)]
struct MockNetworkInner {
    alternate_paths: std::collections::HashMap<PathBuf, PathBuf>,
    recorded_calls: Vec<PathBuf>,
    smb_failures: std::collections::HashMap<PathBuf, String>,
    destination_accessible: bool,
    offline_error: bool,
}

impl Default for MockNetworkInner {
    fn default() -> Self {
        Self {
            alternate_paths: std::collections::HashMap::new(),
            recorded_calls: Vec::new(),
            smb_failures: std::collections::HashMap::new(),
            destination_accessible: true,
            offline_error: false,
        }
    }
}

/// In-memory mock network resolver for testing without network dependencies.
#[derive(Debug, Default, Clone)]
pub struct MockNetworkResolver {
    inner: std::sync::Arc<std::sync::Mutex<MockNetworkInner>>,
}

impl MockNetworkResolver {
    /// Create a new empty `MockNetworkResolver`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an alternate path mapping from `from` to `to`.
    pub fn set_alternate_path(&self, from: impl Into<PathBuf>, to: impl Into<PathBuf>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.alternate_paths.insert(from.into(), to.into());
    }

    /// Configure whether the destination is accessible.
    pub fn set_destination_accessible(&self, accessible: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.destination_accessible = accessible;
    }

    /// Configure whether SMB operations fail with Win32 network offline error code 53.
    pub fn set_offline_error(&self, is_offline: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.offline_error = is_offline;
    }

    /// Configure an SMB connection failure for `unc`.
    pub fn set_smb_failure(&self, unc: impl Into<PathBuf>, err_msg: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.smb_failures.insert(unc.into(), err_msg.into());
    }

    /// Returns recorded paths queried via `try_resolve_alternate_path`.
    pub fn recorded_resolutions(&self) -> Vec<PathBuf> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .recorded_calls
            .clone()
    }
}

impl NetworkResolver for MockNetworkResolver {
    fn try_resolve_alternate_path(&self, path: &Path) -> PathBuf {
        let p = path.to_path_buf();
        let mut inner = self.inner.lock().unwrap_or_else(|l| l.into_inner());
        inner.recorded_calls.push(p.clone());
        if let Some(target) = inner.alternate_paths.get(&p) {
            target.clone()
        } else {
            normalize_path(path)
        }
    }

    fn try_resolve_unc_path(&self, path: &Path) -> PathBuf {
        self.try_resolve_alternate_path(path)
    }

    fn is_destination_accessible(&self, _path: &Path) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|l| l.into_inner())
            .destination_accessible
    }

    fn establish_smb_connection(&self, unc_path: &Path) -> Result<(), SyncError> {
        let p = unc_path.to_path_buf();
        let inner = self.inner.lock().unwrap_or_else(|l| l.into_inner());
        if inner.offline_error {
            return Err(SyncError::Io(std::io::Error::from_raw_os_error(53)));
        }
        if let Some(err_msg) = inner.smb_failures.get(&p) {
            Err(SyncError::validation(err_msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
mod ffi {
    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    #[repr(C)]
    pub struct NETRESOURCEW {
        pub dwScope: u32,
        pub dwType: u32,
        pub dwDisplayType: u32,
        pub dwUsage: u32,
        pub lpLocalName: *const u16,
        pub lpRemoteName: *const u16,
        pub lpComment: *const u16,
        pub lpProvider: *const u16,
    }

    #[link(name = "mpr")]
    unsafe extern "system" {
        pub fn WNetGetConnectionW(
            lpLocalName: *const u16,
            lpRemoteName: *mut u16,
            lpnLength: *mut u32,
        ) -> u32;

        pub fn WNetAddConnection2W(
            lpNetResource: *const NETRESOURCEW,
            lpPassword: *const u16,
            lpUserName: *const u16,
            dwFlags: u32,
        ) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetLogicalDrives() -> u32;
    }
}

/// Query Windows Win32 API `WNetGetConnectionW` to resolve a local drive letter (e.g. "R:")
/// to its underlying remote UNC share path (e.g. "\\\\172.16.0.193\\share").
/// Returns `None` on non-Windows platforms, unmapped drives, or API errors.
#[cfg(target_os = "windows")]
pub(crate) fn resolve_mapped_drive_unc(drive_prefix: &str) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let local_name: Vec<u16> = std::ffi::OsStr::new(drive_prefix)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;

    // SAFETY: `local_name` is a null-terminated UTF-16 wide string pointing to a valid drive prefix.
    // `buf` is a fixed-size stack array with 512 `u16` elements and `len` accurately reflects its capacity.
    // `WNetGetConnectionW` reads from `local_name` up to its null terminator and writes at most `len` elements to `buf`.
    let ret = unsafe { ffi::WNetGetConnectionW(local_name.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if ret == 0 {
        let valid_len = (len as usize).min(buf.len());
        let unc_str = String::from_utf16_lossy(&buf[..valid_len])
            .trim_matches('\0')
            .to_string();
        if !unc_str.is_empty() {
            return Some(unc_str);
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn resolve_mapped_drive_unc(_drive_prefix: &str) -> Option<String> {
    None
}

/// Attempt to convert a path starting with a Windows drive letter into a full UNC network path.
/// If the path starts with a drive letter and `WNetGetConnectionW` succeeds, returns the combined UNC path.
/// Otherwise, returns the original normalized path unchanged.
pub(crate) fn try_resolve_unc_path(path: impl AsRef<Path>) -> PathBuf {
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
///
/// # Errors
/// Returns `SyncError::Validation` if the path is not a valid UNC path or share,
/// or `SyncError::Io` if `WNetAddConnection2W` fails.
#[cfg(target_os = "windows")]
pub(crate) fn establish_smb_connection(unc_path: impl AsRef<Path>) -> Result<(), SyncError> {
    use std::os::windows::ffi::OsStrExt;
    let unc_path = unc_path.as_ref();
    let (host, share) = crate::path_util::parse_unc_host_and_share(unc_path).ok_or_else(|| {
        SyncError::validation(format!(
            "UNC path '{}' does not contain a valid host and share name",
            unc_path.display()
        ))
    })?;

    let unc_share = format!(r"\\{}\{}", host, share);

    let unc_share_w: Vec<u16> = std::ffi::OsStr::new(&unc_share)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let nr = ffi::NETRESOURCEW {
        dwScope: 0,
        dwType: 1, // RESOURCETYPE_DISK
        dwDisplayType: 0,
        dwUsage: 0,
        lpLocalName: std::ptr::null(),
        lpRemoteName: unc_share_w.as_ptr(),
        lpComment: std::ptr::null(),
        lpProvider: std::ptr::null(),
    };

    // SAFETY: `nr.lpRemoteName` points to a null-terminated UTF-16 wide string (`unc_share_w`)
    // that remains valid for the duration of this call. All other pointer fields in NETRESOURCEW
    // and the function arguments are null pointers, which is permitted by WNetAddConnection2W
    // when using default/cached credentials and establishing an unmapped connection.
    let ret = unsafe { ffi::WNetAddConnection2W(&nr, std::ptr::null(), std::ptr::null(), 0) };

    // 0 = NO_ERROR, 85 = ERROR_ALREADY_ASSIGNED, 1219 = ERROR_SESSION_CREDENTIAL_CONFLICT
    if ret == 0 || ret == 85 || ret == 1219 {
        Ok(())
    } else {
        Err(SyncError::Io(std::io::Error::from_raw_os_error(ret as i32)))
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn establish_smb_connection(unc_path: impl AsRef<Path>) -> Result<(), SyncError> {
    let unc_path = unc_path.as_ref();
    if crate::path_util::parse_unc_host_and_share(unc_path).is_none() {
        return Err(SyncError::validation(format!(
            "UNC path '{}' does not contain a valid host and share name",
            unc_path.display()
        )));
    }
    Err(SyncError::validation(
        "SMB connection is only supported on Windows",
    ))
}

/// Find the byte boundary in `original` where the lowercase representation matches `lower_prefix`.
///
/// Returns `None` if `lower_prefix` does not match the lowercase prefix of `original`
/// or cuts across a multi-character lowercase expansion.
fn find_prefix_byte_boundary(original: &str, lower_prefix: &str) -> Option<usize> {
    if !original.to_lowercase().starts_with(lower_prefix) {
        return None;
    }
    let mut accumulated_lower_len = 0;
    for (idx, ch) in original.char_indices() {
        if accumulated_lower_len == lower_prefix.len() {
            return Some(idx);
        }
        if accumulated_lower_len > lower_prefix.len() {
            return None;
        }
        for lower_ch in ch.to_lowercase() {
            accumulated_lower_len += lower_ch.len_utf8();
        }
    }
    if accumulated_lower_len == lower_prefix.len() {
        Some(original.len())
    } else {
        None
    }
}

/// Reverse-lookup active Win32 mapped drive letters ('A'..='Z') to find a drive letter
/// mapped to a prefix of the given UNC path.
pub(crate) fn find_mapped_drive_for_unc(unc_path: impl AsRef<Path>) -> Option<PathBuf> {
    let normalized = normalize_path(unc_path);
    let original_str = normalized.to_string_lossy();
    let unc_str_lower = original_str.to_lowercase();
    if !unc_str_lower.starts_with(r"\\") {
        return None;
    }

    #[cfg(target_os = "windows")]
    let drive_mask = unsafe { ffi::GetLogicalDrives() };
    #[cfg(not(target_os = "windows"))]
    let drive_mask = 0u32;

    let mut drive_buf = [0u8; 2];
    drive_buf[1] = b':';

    for i in 0..26 {
        #[cfg(target_os = "windows")]
        if (drive_mask & (1 << i)) == 0 {
            continue;
        }
        drive_buf[0] = b'A' + i as u8;
        let drive_prefix = match std::str::from_utf8(&drive_buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(mapped_unc) = resolve_mapped_drive_unc(drive_prefix) {
            let mapped_lower = mapped_unc.trim_end_matches('\\').to_lowercase();
            if !mapped_lower.is_empty() && unc_str_lower.starts_with(&mapped_lower) {
                let rest = match find_prefix_byte_boundary(&original_str, &mapped_lower) {
                    Some(idx) => &original_str[idx..],
                    None => continue,
                };
                if rest.is_empty() || rest.starts_with('\\') {
                    let relative = rest.trim_start_matches('\\');
                    if relative.is_empty() {
                        return Some(PathBuf::from(format!(r"{}\", drive_prefix)));
                    } else {
                        return Some(PathBuf::from(format!(r"{}\{}", drive_prefix, relative)));
                    }
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
pub(crate) fn try_resolve_alternate_path(path: impl AsRef<Path>) -> PathBuf {
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
            if let Err(e) = establish_smb_connection(&unc_path) {
                tracing::debug!(
                    target = %unc_path.display(),
                    error = %e,
                    "SMB session establishment failed during drive letter resolution"
                );
            }
            if matches!(std::fs::metadata(&unc_path), Ok(m) if m.is_dir()) {
                return unc_path;
            }
        }
    }

    // Case 2: UNC path (e.g. "\\172.16.0.193\Files")
    if s.starts_with(r"\\") {
        // Attempt SMB session establishment on UNC path
        if let Err(e) = establish_smb_connection(&normalized) {
            tracing::debug!(
                target = %normalized.display(),
                error = %e,
                "SMB session establishment failed during UNC path resolution"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_mapped_drive_for_unc_unicode_safe() {
        let test_paths = [
            Path::new(r"\\server\share\föö\bär\файл.dat"),
            Path::new(r"\\10.0.0.1\分享\数据\测试.txt"),
            Path::new(r"\\server\share\🚀\item"),
            Path::new(r"\\server\share\café\résumé.doc"),
        ];

        for path in &test_paths {
            let result = std::panic::catch_unwind(|| find_mapped_drive_for_unc(path));
            assert!(
                result.is_ok(),
                "find_mapped_drive_for_unc panicked on UTF-8 path: {}",
                path.display()
            );
        }
    }

    #[test]
    fn test_find_prefix_byte_boundary_unicode() {
        let orig = "föö_BÄR_test";
        assert_eq!(find_prefix_byte_boundary(orig, "föö"), Some(5));
        assert_eq!(find_prefix_byte_boundary(orig, "föö_bär"), Some(10));
        assert_eq!(
            find_prefix_byte_boundary(orig, "föö_bär_test"),
            Some(orig.len())
        );
        assert_eq!(find_prefix_byte_boundary(orig, "nonexistent"), None);
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
    fn test_mock_network_resolver_alternate_path() {
        let resolver = MockNetworkResolver::new();
        resolver.set_alternate_path(r"R:\data", r"\\server\share\data");

        let resolved = resolver.try_resolve_alternate_path(Path::new(r"R:\data"));
        assert_eq!(resolved, PathBuf::from(r"\\server\share\data"));

        let unmapped = resolver.try_resolve_alternate_path(Path::new(r"C:\unmapped"));
        assert_eq!(unmapped, PathBuf::from(r"C:\unmapped"));

        let recorded = resolver.recorded_resolutions();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0], PathBuf::from(r"R:\data"));
        assert_eq!(recorded[1], PathBuf::from(r"C:\unmapped"));
    }

    #[test]
    fn test_mock_network_resolver_smb_failure() {
        let resolver = MockNetworkResolver::new();
        resolver.set_smb_failure(r"\\offline\share", "network unreachable");

        assert!(
            resolver
                .establish_smb_connection(Path::new(r"\\offline\share"))
                .is_err()
        );
        assert!(
            resolver
                .establish_smb_connection(Path::new(r"\\online\share"))
                .is_ok()
        );
    }

    #[test]
    fn test_mock_network_resolver_reachability_and_offline_codes() {
        let resolver = MockNetworkResolver::new();
        // Reachability is true by default
        assert!(resolver.is_destination_accessible(Path::new(r"C:\test")));

        // Reachability can be set to false
        resolver.set_destination_accessible(false);
        assert!(!resolver.is_destination_accessible(Path::new(r"C:\test")));

        // Offline error can be toggled
        assert!(
            resolver
                .establish_smb_connection(Path::new(r"\\server\share"))
                .is_ok()
        );
        resolver.set_offline_error(true);
        let err = resolver
            .establish_smb_connection(Path::new(r"\\server\share"))
            .unwrap_err();
        assert!(err.is_network_offline());
        match err {
            SyncError::Io(io_err) => assert_eq!(io_err.raw_os_error(), Some(53)),
            other => panic!("Expected SyncError::Io, got {:?}", other),
        }

        // Win32 resolver default implementation test
        let win32 = Win32NetworkResolver;
        assert!(!win32.is_destination_accessible(Path::new(r"C:\non_existent_folder_xyz_98765")));
    }
}

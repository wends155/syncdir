//! Network and Win32 UNC/SMB connection management.
//!
//! Provides utilities for querying mapped drive UNC targets, establishing SMB sessions,
//! and resolving alternate network paths.

use crate::config::normalize_path;
use crate::error::SyncError;
use std::path::{Path, PathBuf};

/// Query Windows Win32 API `WNetGetConnectionW` to resolve a local drive letter (e.g. "R:")
/// to its underlying remote UNC share path (e.g. "\\\\172.16.0.193\\share").
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

    // SAFETY: `local_name` is a null-terminated UTF-16 wide string pointing to a valid drive prefix.
    // `buf` is pre-allocated with 512 `u16` elements and `len` accurately reflects its capacity.
    // `WNetGetConnectionW` reads from `local_name` up to its null terminator and writes at most `len` elements to `buf`.
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
///
/// # Errors
/// Returns `SyncError::Validation` if the path is not a valid UNC path or share,
/// or `SyncError::Io` if `WNetAddConnection2W` fails.
#[cfg(target_os = "windows")]
pub fn establish_smb_connection(unc_path: &Path) -> Result<(), SyncError> {
    use std::os::windows::ffi::OsStrExt;
    let s = unc_path.to_string_lossy();
    if !s.starts_with(r"\\") {
        return Err(SyncError::Validation(format!(
            "Path '{}' is not a UNC network path",
            unc_path.display()
        )));
    }

    // Extract root share e.g. "\\172.16.0.193\Files" or "\\172.16.0.193\ABB Industrial IT Data"
    let parts: Vec<&str> = s[2..].split('\\').collect();
    if parts.len() < 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
        return Err(SyncError::validation(format!(
            "UNC path '{}' does not contain a valid host and share name",
            unc_path.display()
        )));
    }
    let unc_share = format!(r"\\{}\{}", parts[0], parts[1]);

    let unc_share_w: Vec<u16> = std::ffi::OsStr::new(&unc_share)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Win32 FFI: field names and struct name must match the Windows API naming convention (NETRESOURCEW).
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

    // SAFETY: `nr.lpRemoteName` points to a null-terminated UTF-16 wide string (`unc_share_w`)
    // that remains valid for the duration of this call. All other pointer fields in NETRESOURCEW
    // and the function arguments are null pointers, which is permitted by WNetAddConnection2W
    // when using default/cached credentials and establishing an unmapped connection.
    let ret = unsafe { WNetAddConnection2W(&nr, std::ptr::null(), std::ptr::null(), 0) };

    // 0 = NO_ERROR, 85 = ERROR_ALREADY_ASSIGNED, 1219 = ERROR_SESSION_CREDENTIAL_CONFLICT
    if ret == 0 || ret == 85 || ret == 1219 {
        Ok(())
    } else {
        Err(SyncError::Io(std::io::Error::from_raw_os_error(ret as i32)))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn establish_smb_connection(_unc_path: &Path) -> Result<(), SyncError> {
    Err(SyncError::Validation(
        "SMB connection is only supported on Windows".into(),
    ))
}

/// Reverse-lookup active Win32 mapped drive letters ('A'..='Z') to find a drive letter
/// mapped to a prefix of the given UNC path.
pub fn find_mapped_drive_for_unc(unc_path: &Path) -> Option<PathBuf> {
    let normalized = normalize_path(unc_path);
    let original_str = normalized.to_string_lossy();
    let unc_str_lower = original_str.to_lowercase();
    if !unc_str_lower.starts_with(r"\\") {
        return None;
    }

    for letter in (b'A'..=b'Z').map(|b| b as char) {
        let drive_prefix = format!("{}:", letter);
        if let Some(mapped_unc) = resolve_mapped_drive_unc(&drive_prefix) {
            let mapped_lower = mapped_unc.trim_end_matches('\\').to_lowercase();
            if !mapped_lower.is_empty() && unc_str_lower.starts_with(&mapped_lower) {
                let rest_idx = mapped_lower.len();
                let rest = &original_str[rest_idx..];
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
            let _ = establish_smb_connection(&unc_path);
            if matches!(std::fs::metadata(&unc_path), Ok(m) if m.is_dir()) {
                return unc_path;
            }
        }
    }

    // Case 2: UNC path (e.g. "\\172.16.0.193\Files")
    if s.starts_with(r"\\") {
        // Attempt SMB session establishment on UNC path
        let _ = establish_smb_connection(&normalized);
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

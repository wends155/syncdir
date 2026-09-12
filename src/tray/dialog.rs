//! Modal dialogs and external shell process launching for the system tray interface.

use std::path::Path;

/// Encode a UTF-8 string as a null-terminated UTF-16 wide string for Win32 API calls.
pub(crate) fn to_wide_null_terminated(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Display a native Windows About modal dialog box containing version, description, copyright, and URL.
#[cfg(target_os = "windows")]
pub(crate) fn show_about_dialog() {
    let res = std::thread::Builder::new()
        .name("about-dialog".to_string())
        .spawn(|| {
            let title = to_wide_null_terminated("About syncdir");
            let msg_text = format!(
                "syncdir v{} — Windows background folder synchronization daemon\n{}\n{}",
                env!("CARGO_PKG_VERSION"),
                crate::COPYRIGHT,
                env!("CARGO_PKG_REPOSITORY")
            );
            let text = to_wide_null_terminated(&msg_text);
            // SAFETY: MessageBoxW is a standard Win32 API function. Passing null hwnd and valid
            // null-terminated wide character array pointers is safe and opens a native modal dialog.
            unsafe {
                unsafe extern "system" {
                    fn MessageBoxW(
                        hwnd: *mut std::ffi::c_void,
                        text: *const u16,
                        caption: *const u16,
                        utype: u32,
                    ) -> i32;
                }
                MessageBoxW(
                    std::ptr::null_mut(),
                    text.as_ptr(),
                    title.as_ptr(),
                    0x00000040,
                ); // MB_OK | MB_ICONINFORMATION
            }
        });
    if let Err(e) = res {
        tracing::error!(error = %e, "Failed to spawn thread for about dialog");
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn show_about_dialog() {}

/// Display a native Windows Error modal dialog box.
#[cfg(target_os = "windows")]
pub(crate) fn show_error_dialog(title_str: &str, msg_str: &str) {
    let title_owned = title_str.to_string();
    let msg_owned = msg_str.to_string();
    let res = std::thread::Builder::new()
        .name("error-dialog".to_string())
        .spawn(move || {
            let title_wide = to_wide_null_terminated(&title_owned);
            let msg_wide = to_wide_null_terminated(&msg_owned);
            // SAFETY: MessageBoxW is a standard Win32 API function.
            unsafe {
                unsafe extern "system" {
                    fn MessageBoxW(
                        hwnd: *mut std::ffi::c_void,
                        text: *const u16,
                        caption: *const u16,
                        utype: u32,
                    ) -> i32;
                }
                MessageBoxW(
                    std::ptr::null_mut(),
                    msg_wide.as_ptr(),
                    title_wide.as_ptr(),
                    0x00000010,
                ); // MB_OK | MB_ICONERROR
            }
        });
    if let Err(e) = res {
        tracing::error!(
            error = %e,
            title = %title_str,
            msg = %msg_str,
            "Failed to spawn thread for error dialog"
        );
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn show_error_dialog(_title_str: &str, _msg_str: &str) {}

/// Format command line arguments for Windows Explorer to navigate directories
/// or highlight files with `/select`.
///
/// When a file path contains whitespace, formats `/select,"<path>"` to prevent
/// Windows Explorer argument parser ambiguity.
#[must_use]
pub fn format_explorer_args(path: &Path, is_dir: bool) -> Vec<std::ffi::OsString> {
    if is_dir {
        vec![path.as_os_str().to_os_string()]
    } else {
        let has_spaces = path.to_string_lossy().contains(' ');
        let mut arg = std::ffi::OsString::new();
        if has_spaces {
            arg.push("/select,\"");
            arg.push(path.as_os_str());
            arg.push("\"");
        } else {
            arg.push("/select,");
            arg.push(path.as_os_str());
        }
        vec![arg]
    }
}

/// Launches the system file explorer targeting the specified path.
///
/// Returns `Err(std::io::Error)` with `ErrorKind::NotFound` if the path does not exist
/// or if explorer is not found.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Path does not exist: {}", path.display()),
        ));
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let explorer = crate::path_util::system_root().join("explorer.exe");
        if !explorer.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Explorer executable not found at {}", explorer.display()),
            ));
        }
        let args = format_explorer_args(path, path.is_dir());
        let mut cmd = std::process::Command::new(explorer);
        for arg in args {
            if arg.to_string_lossy().starts_with("/select,\"") {
                cmd.raw_arg(arg);
            } else {
                cmd.arg(arg);
            }
        }
        cmd.spawn()?;
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_format_explorer_args_directory() {
        let dir_path = Path::new(r"C:\Program Files\SyncDir");
        let args_dir = format_explorer_args(dir_path, true);
        assert_eq!(args_dir.len(), 1);
        assert_eq!(
            args_dir[0],
            std::ffi::OsString::from(r"C:\Program Files\SyncDir")
        );
    }

    #[test]
    fn test_format_explorer_args_file_without_whitespace() {
        let file_path = Path::new(r"C:\folder\file.txt");
        let args_file = format_explorer_args(file_path, false);
        assert_eq!(args_file.len(), 1);
        assert_eq!(
            args_file[0],
            std::ffi::OsString::from(r"/select,C:\folder\file.txt")
        );
    }

    #[test]
    fn test_format_explorer_args_file_with_whitespace() {
        let file_path = Path::new(r"C:\Program Files\App Data\log file.txt");
        let args_file = format_explorer_args(file_path, false);
        assert_eq!(args_file.len(), 1);
        assert_eq!(
            args_file[0],
            std::ffi::OsString::from(r#"/select,"C:\Program Files\App Data\log file.txt""#)
        );
    }

    #[test]
    fn test_tray_open_path_nonexistent() {
        let res = open_path(Path::new("Z:\\nonexistent_dir_12345\\missing"));
        assert!(res.is_err());
    }

    #[test]
    fn test_tray_open_path_nonexistent_returns_not_found() {
        let non_existent = std::path::Path::new(r"C:\definitely_does_not_exist_tray_test_12345");
        let res = open_path(non_existent);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn test_to_wide_null_terminated_ascii() {
        let wide = to_wide_null_terminated("Hello");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(wide.len(), 6);
        assert_eq!(wide[0], 'H' as u16);
    }

    #[test]
    fn test_to_wide_null_terminated_empty() {
        let wide = to_wide_null_terminated("");
        assert_eq!(wide, vec![0]);
    }

    #[test]
    fn test_to_wide_null_terminated_unicode_and_crlf() {
        let wide = to_wide_null_terminated("⚠️\r\nTest\t");
        assert_eq!(wide.last(), Some(&0));
        assert!(wide.len() > 6);
    }

    #[test]
    fn test_show_about_dialog_does_not_panic() {
        show_about_dialog();
    }

    #[test]
    #[ignore = "Spawns interactive Win32 modal dialog; manual verification only"]
    fn test_show_error_dialog_manual() {
        show_error_dialog("Test Title", "Test Message");
    }
}

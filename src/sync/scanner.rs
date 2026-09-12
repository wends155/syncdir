use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::TargetSyncConfig;
use crate::error::SyncError;

use super::path_safety::is_reparse_or_symlink;

pub(crate) fn scan_dir_cancellable(
    dir: &Path,
    source_root: &Path,
    files: &mut HashSet<PathBuf>,
    scan_complete: &mut bool,
    depth: usize,
    cancel: &AtomicBool,
) -> Result<(), SyncError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(SyncError::Cancelled);
    }
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        tracing::warn!(path = ?dir, "Max directory depth exceeded, skipping");
        *scan_complete = false;
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!(path = ?dir, error = %e, "Permission denied scanning directory; skipping");
            *scan_complete = false;
            return Ok(());
        }
        Err(e) => return Err(SyncError::Io(e)),
    };
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::warn!(error = %e, "Permission denied reading directory entry; skipping");
                *scan_complete = false;
                continue;
            }
            Err(e) => return Err(SyncError::Io(e)),
        };
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::warn!(dir = ?dir, error = %e, "Permission denied querying file type; skipping");
                *scan_complete = false;
                continue;
            }
            Err(e) => return Err(SyncError::Io(e)),
        };
        if !file_type.is_dir() && !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        match is_reparse_or_symlink(&entry, &path) {
            Ok(true) => {
                tracing::debug!(path = ?path, "Skipping reparse point or symlink in scan");
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(path = ?path, error = %e, "Failed checking reparse point; skipping entry and marking scan incomplete");
                *scan_complete = false;
                continue;
            }
        }
        if file_type.is_dir() {
            scan_dir_cancellable(&path, source_root, files, scan_complete, depth + 1, cancel)?;
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(source_root)
        {
            files.insert(rel.to_path_buf());
        }
    }
    Ok(())
}

pub(crate) fn scan_dir(
    dir: &Path,
    source_root: &Path,
    files: &mut HashSet<PathBuf>,
    scan_complete: &mut bool,
    depth: usize,
) -> Result<(), std::io::Error> {
    static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
    match scan_dir_cancellable(
        dir,
        source_root,
        files,
        scan_complete,
        depth,
        &NEVER_CANCELLED,
    ) {
        Ok(()) => Ok(()),
        Err(SyncError::Io(e)) => Err(e),
        Err(_) => Err(std::io::Error::other("scan_dir failed")),
    }
}

/// Dedicated collaborating scanner for recursive directory traversal and cancellation.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryScanner {
    config: TargetSyncConfig,
}

impl DirectoryScanner {
    pub(crate) fn new(config: TargetSyncConfig) -> Self {
        Self { config }
    }

    /// Perform a cancellable scan of the source directory, populating `files` with relative paths.
    #[tracing::instrument(
        skip(self, files, scan_complete, cancel),
        fields(source_root = ?source_root),
        level = "debug"
    )]
    pub(crate) fn scan_dir_cancellable(
        &self,
        source_root: &Path,
        files: &mut HashSet<PathBuf>,
        scan_complete: &mut bool,
        cancel: &AtomicBool,
    ) -> Result<(), SyncError> {
        scan_dir_cancellable(source_root, source_root, files, scan_complete, 0, cancel)
    }

    /// Return a reference to the configured target sync settings.
    #[allow(dead_code)]
    pub(crate) fn config(&self) -> &TargetSyncConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use tempfile::tempdir;

    #[test]
    fn test_scan_dir_skips_symlinks() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("real.txt"), "content").unwrap();
        #[cfg(windows)]
        {
            let external = tmp.path().join("external");
            fs::create_dir_all(&external).unwrap();
            fs::write(external.join("secret.txt"), "sensitive").unwrap();
            let _ = std::os::windows::fs::symlink_dir(&external, src.join("link"));
        }
        let mut files = std::collections::HashSet::new();
        let mut scan_complete = true;
        scan_dir(&src, &src, &mut files, &mut scan_complete, 0).unwrap();
        assert!(scan_complete);
        assert!(files.contains(Path::new("real.txt")));
        assert!(!files.iter().any(|f| f.to_string_lossy().contains("secret")));
    }

    #[test]
    fn test_scan_dir_normal_and_max_depth() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("test.txt"), b"123").unwrap();

        let mut files = HashSet::new();
        let mut scan_complete = true;
        assert!(scan_dir(&root, &root, &mut files, &mut scan_complete, 0).is_ok());
        assert_eq!(files.len(), 1);
        assert!(scan_complete);

        let mut files_skipped = HashSet::new();
        let mut scan_complete_skipped = true;
        assert!(
            scan_dir(
                &root,
                &root,
                &mut files_skipped,
                &mut scan_complete_skipped,
                65
            )
            .is_ok()
        );
        assert_eq!(files_skipped.len(), 0);
        assert!(!scan_complete_skipped);
    }

    #[test]
    fn test_scan_dir_permission_denied_marks_incomplete() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let mut files = HashSet::new();
        let mut scan_complete = true;
        scan_dir(&root, &root, &mut files, &mut scan_complete, 65).unwrap();
        assert!(!scan_complete);
    }

    #[test]
    fn test_directory_scanner_standalone() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("file1.txt"), b"1").unwrap();
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("nested").join("file2.txt"), b"2").unwrap();

        let config = Config::test_default(src.clone(), dst);
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let scanner = DirectoryScanner::new(target_cfg);

        let mut files = HashSet::new();
        let mut scan_complete = true;
        let cancel = AtomicBool::new(false);
        scanner
            .scan_dir_cancellable(&src, &mut files, &mut scan_complete, &cancel)
            .unwrap();

        assert!(scan_complete);
        assert_eq!(files.len(), 2);
        assert!(files.contains(Path::new("file1.txt")));
        assert!(files.contains(&Path::new("nested").join("file2.txt")));
    }

    #[test]
    fn test_directory_scanner_discovers_files_and_ignores_junctions() {
        use crate::config::{Config, TargetSyncConfig};
        use std::collections::HashSet;
        use std::fs;
        use std::path::Path;
        use std::sync::atomic::AtomicBool;
        use tempfile::tempdir;

        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        let outside = temp.path().join("outside_target");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::create_dir_all(&outside).unwrap();

        fs::write(src.join("root.txt"), b"root content").unwrap();
        let sub = src.join("nested_folder");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("nested_file.txt"), b"nested content").unwrap();
        fs::write(outside.join("secret_data.txt"), b"confidential").unwrap();

        #[cfg(windows)]
        {
            let junction_path = src.join("external_junction");
            let _ = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "New-Item -ItemType Junction -Path '{}' -Target '{}' -Force",
                        junction_path.display(),
                        outside.display()
                    ),
                ])
                .status();
        }

        let config = Config::test_default(src.clone(), dst);
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let scanner = DirectoryScanner::new(target_cfg);

        let mut files = HashSet::new();
        let mut scan_complete = true;
        let cancel = AtomicBool::new(false);

        scanner
            .scan_dir_cancellable(&src, &mut files, &mut scan_complete, &cancel)
            .expect("Directory scanner must execute successfully");

        assert!(scan_complete);
        assert!(files.contains(Path::new("root.txt")));
        assert!(files.contains(&Path::new("nested_folder").join("nested_file.txt")));
        assert!(
            !files
                .iter()
                .any(|p| p.to_string_lossy().contains("secret_data")),
            "Directory scanner must strictly ignore files located behind reparse junctions"
        );
    }

    #[test]
    fn test_scanner_logs_use_debug_path_formatting_cwe_117() {
        use crate::test_support::with_captured_tracing;
        use std::collections::HashSet;

        let temp = tempfile::tempdir().expect("tempdir");
        let mut files = HashSet::new();
        let mut scan_complete = true;

        let (_, logs) = with_captured_tracing(|| {
            // Depth 65 exceeds MAX_DEPTH (64) triggering depth warning
            let _ = scan_dir(temp.path(), temp.path(), &mut files, &mut scan_complete, 65);
        });

        assert!(
            logs.contains("path = \"") || logs.contains("path=\""),
            "Scanner logs must format path using Debug (?dir / ?path) to quote and escape CRLF injection, got: {}",
            logs
        );
        assert!(
            !logs.contains("\r\n"),
            "Logs must not contain unescaped CRLF line breaks"
        );
    }
}

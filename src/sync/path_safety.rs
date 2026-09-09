use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

use crate::error::SyncError;

#[cfg(windows)]
pub(crate) fn verify_destination_not_reparse(
    dest_dir: &Path,
    rel_path: &Path,
) -> Result<Option<Metadata>, SyncError> {
    use std::os::windows::fs::MetadataExt;
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    for component in rel_path.components() {
        current.push(component);
        if let Ok(m) = fs::symlink_metadata(&current) {
            if (m.file_attributes() & 0x400) != 0 || m.file_type().is_symlink() {
                return Err(SyncError::validation(format!(
                    "Destination component '{}' is a symlink or reparse point; refusing to write",
                    current.display()
                )));
            }
            leaf_meta = Some(m);
        } else {
            leaf_meta = None;
        }
    }
    Ok(leaf_meta)
}

#[cfg(not(windows))]
pub(crate) fn verify_destination_not_reparse(
    dest_dir: &Path,
    rel_path: &Path,
) -> Result<Option<Metadata>, SyncError> {
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    for component in rel_path.components() {
        current.push(component);
        if let Ok(m) = fs::symlink_metadata(&current) {
            if m.file_type().is_symlink() {
                return Err(SyncError::validation(format!(
                    "Destination component '{}' is a symlink; refusing to write",
                    current.display()
                )));
            }
            leaf_meta = Some(m);
        } else {
            leaf_meta = None;
        }
    }
    Ok(leaf_meta)
}

#[cfg(windows)]
#[doc(hidden)]
pub fn verify_destination_not_reparse_cached(
    dest_dir: &Path,
    rel_path: &Path,
    verified_dirs: &mut HashSet<PathBuf>,
) -> Result<Option<Metadata>, SyncError> {
    use std::os::windows::fs::MetadataExt;
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    for component in rel_path.components() {
        current.push(component);
        if verified_dirs.contains(&current) {
            if let Ok(m) = fs::symlink_metadata(&current) {
                leaf_meta = Some(m);
            } else {
                leaf_meta = None;
            }
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if (m.file_attributes() & 0x400) != 0 || m.file_type().is_symlink() {
                return Err(SyncError::validation(format!(
                    "Destination component '{}' is a symlink or reparse point; refusing to write",
                    current.display()
                )));
            }
            if m.is_dir() {
                verified_dirs.insert(current.clone());
            }
            leaf_meta = Some(m);
        } else {
            leaf_meta = None;
        }
    }
    Ok(leaf_meta)
}

#[cfg(not(windows))]
#[doc(hidden)]
pub fn verify_destination_not_reparse_cached(
    dest_dir: &Path,
    rel_path: &Path,
    verified_dirs: &mut HashSet<PathBuf>,
) -> Result<Option<Metadata>, SyncError> {
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    for component in rel_path.components() {
        current.push(component);
        if verified_dirs.contains(&current) {
            if let Ok(m) = fs::symlink_metadata(&current) {
                leaf_meta = Some(m);
            } else {
                leaf_meta = None;
            }
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if m.file_type().is_symlink() {
                return Err(SyncError::validation(format!(
                    "Destination component '{}' is a symlink; refusing to write",
                    current.display()
                )));
            }
            if m.is_dir() {
                verified_dirs.insert(current.clone());
            }
            leaf_meta = Some(m);
        } else {
            leaf_meta = None;
        }
    }
    Ok(leaf_meta)
}

#[cfg(windows)]
pub(crate) fn verify_source_not_reparse(
    source_dir: &Path,
    rel_path: &Path,
) -> Result<(), SyncError> {
    let mut current = source_dir.to_path_buf();
    for component in rel_path.parent().into_iter().flat_map(|p| p.components()) {
        current.push(component);
        if let Ok(m) = fs::symlink_metadata(&current)
            && is_reparse_or_symlink_meta(&m)
        {
            return Err(SyncError::validation(format!(
                "Source ancestor '{}' is a symlink or reparse point; refusing to read",
                current.display()
            )));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn verify_source_not_reparse(
    source_dir: &Path,
    rel_path: &Path,
) -> Result<(), SyncError> {
    let mut current = source_dir.to_path_buf();
    for component in rel_path.parent().into_iter().flat_map(|p| p.components()) {
        current.push(component);
        if let Ok(m) = fs::symlink_metadata(&current)
            && is_reparse_or_symlink_meta(&m)
        {
            return Err(SyncError::validation(format!(
                "Source ancestor '{}' is a symlink; refusing to read",
                current.display()
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn is_reparse_or_symlink_meta(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    (meta.file_attributes() & 0x400) != 0 || meta.file_type().is_symlink()
}

#[cfg(not(windows))]
pub(crate) fn is_reparse_or_symlink_meta(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

#[cfg(windows)]
pub(crate) fn is_reparse_or_symlink(entry: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    match fs::symlink_metadata(entry.path()) {
        Ok(meta) => (meta.file_attributes() & 0x400) != 0 || meta.file_type().is_symlink(),
        Err(_) => true,
    }
}

#[cfg(not(windows))]
pub(crate) fn is_reparse_or_symlink(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(true)
}

#[doc(hidden)]
pub fn is_safe_relative_path(path: &Path) -> bool {
    if !path.is_relative() || path.as_os_str().is_empty() {
        return false;
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
        "CONOUT$", "CLOCK$",
    ];
    for component in path.components() {
        match component {
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return false,
            std::path::Component::Normal(os_str) => {
                let s = os_str.to_string_lossy();
                if s.contains(':') {
                    return false;
                }
                // Reject components with trailing spaces or dots (Windows strips these)
                if s.ends_with(' ') || s.ends_with('.') {
                    return false;
                }
                // Trim trailing spaces and dots before reserved name check
                let trimmed = s.trim_end_matches([' ', '.']);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_is_safe_relative_path_rejects_empty() {
        assert!(!is_safe_relative_path(Path::new("")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_reserved_names() {
        assert!(!is_safe_relative_path(Path::new("CON")));
        assert!(!is_safe_relative_path(Path::new("subdir\\NUL.txt")));
        assert!(!is_safe_relative_path(Path::new("COM1")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_ads() {
        assert!(!is_safe_relative_path(Path::new("file.txt:stream")));
    }

    #[test]
    fn test_dos_device_trailing_space_dot() {
        // Trailing space bypass
        assert!(!is_safe_relative_path(Path::new("CON ")));
        assert!(!is_safe_relative_path(Path::new("NUL ")));
        assert!(!is_safe_relative_path(Path::new("COM1 ")));
        // Trailing dot bypass
        assert!(!is_safe_relative_path(Path::new("CON.")));
        assert!(!is_safe_relative_path(Path::new("file.txt.")));
        // Trailing dot on directory component
        assert!(!is_safe_relative_path(Path::new("subdir./file.txt")));
        // Valid paths still pass
        assert!(is_safe_relative_path(Path::new("normal_file.txt")));
        assert!(is_safe_relative_path(Path::new("subdir/file.txt")));
    }

    #[test]
    fn test_verify_destination_not_reparse_ancestors() {
        let temp = tempdir().unwrap();
        let dest_dir = temp.path();
        let rel_path = Path::new("sub/dir/nested/file.txt");

        // When intermediate dirs do not exist yet, it succeeds
        assert!(verify_destination_not_reparse(dest_dir, rel_path).is_ok());

        // When intermediate dirs are normal directories, it succeeds
        std::fs::create_dir_all(dest_dir.join("sub/dir/nested")).unwrap();
        assert!(verify_destination_not_reparse(dest_dir, rel_path).is_ok());
    }

    #[test]
    fn test_reparse_cache_hit() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        let deep = dest.join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("file1.txt"), b"1").unwrap();
        fs::write(deep.join("file2.txt"), b"2").unwrap();

        let mut cache = HashSet::new();
        let meta1 =
            verify_destination_not_reparse_cached(&dest, Path::new("a/b/c/file1.txt"), &mut cache)
                .unwrap();
        assert!(meta1.is_some());
        assert!(!cache.is_empty());
        let count_before = cache.len();

        // Second file in same directory should hit cache for all ancestors
        let meta2 =
            verify_destination_not_reparse_cached(&dest, Path::new("a/b/c/file2.txt"), &mut cache)
                .unwrap();
        assert!(meta2.is_some());
        assert_eq!(cache.len(), count_before);
    }
}

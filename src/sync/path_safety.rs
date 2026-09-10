use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

use crate::error::SyncError;

#[cfg(windows)]
pub(crate) fn verify_destination_not_reparse(
    dest_dir: &Path,
    rel_path: &Path,
) -> Result<Option<Metadata>, SyncError> {
    if fs::symlink_metadata(dest_dir)
        .map(|m| is_reparse_or_symlink_meta(&m))
        .unwrap_or(false)
    {
        return Err(SyncError::validation(format!(
            "Destination root '{}' is a symlink or reparse point; refusing to write",
            dest_dir.display()
        )));
    }
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    for component in rel_path.components() {
        current.push(component);
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
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
    if fs::symlink_metadata(dest_dir)
        .map(|m| is_reparse_or_symlink_meta(&m))
        .unwrap_or(false)
    {
        return Err(SyncError::validation(format!(
            "Destination root '{}' is a symlink; refusing to write",
            dest_dir.display()
        )));
    }
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    for component in rel_path.components() {
        current.push(component);
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
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
    if !verified_dirs.contains(dest_dir) {
        if fs::symlink_metadata(dest_dir)
            .map(|m| is_reparse_or_symlink_meta(&m))
            .unwrap_or(false)
        {
            return Err(SyncError::validation(format!(
                "Destination root '{}' is a symlink or reparse point; refusing to write",
                dest_dir.display()
            )));
        }
        if verified_dirs.len() >= 1000 {
            verified_dirs.clear();
        }
        verified_dirs.insert(dest_dir.to_path_buf());
    }
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    let components: Vec<_> = rel_path.components().collect();
    let total = components.len();
    for (i, component) in components.into_iter().enumerate() {
        let is_leaf = i + 1 == total;
        current.push(component);
        if verified_dirs.contains(&current) {
            if is_leaf {
                leaf_meta = fs::symlink_metadata(&current).ok();
            }
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation(format!(
                    "Destination component '{}' is a symlink or reparse point; refusing to write",
                    current.display()
                )));
            }
            if m.is_dir() {
                if verified_dirs.len() >= 1000 {
                    verified_dirs.clear();
                }
                verified_dirs.insert(current.clone());
            }
            if is_leaf {
                leaf_meta = Some(m);
            }
        } else if is_leaf {
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
    if !verified_dirs.contains(dest_dir) {
        if let Ok(m) = fs::symlink_metadata(dest_dir) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation(format!(
                    "Destination root '{}' is a symlink; refusing to write",
                    dest_dir.display()
                )));
            }
        }
        if verified_dirs.len() >= 1000 {
            verified_dirs.clear();
        }
        verified_dirs.insert(dest_dir.to_path_buf());
    }
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    let components: Vec<_> = rel_path.components().collect();
    let total = components.len();
    for (i, component) in components.into_iter().enumerate() {
        let is_leaf = i + 1 == total;
        current.push(component);
        if verified_dirs.contains(&current) {
            if is_leaf {
                leaf_meta = fs::symlink_metadata(&current).ok();
            }
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation(format!(
                    "Destination component '{}' is a symlink; refusing to write",
                    current.display()
                )));
            }
            if m.is_dir() {
                if verified_dirs.len() >= 1000 {
                    verified_dirs.clear();
                }
                verified_dirs.insert(current.clone());
            }
            if is_leaf {
                leaf_meta = Some(m);
            }
        } else if is_leaf {
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
pub(crate) fn is_reparse_or_symlink(entry: &std::fs::DirEntry) -> Result<bool, std::io::Error> {
    use std::os::windows::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(entry.path())?;
    Ok((meta.file_attributes() & 0x400) != 0 || meta.file_type().is_symlink())
}

#[cfg(not(windows))]
pub(crate) fn is_reparse_or_symlink(entry: &std::fs::DirEntry) -> Result<bool, std::io::Error> {
    Ok(entry.file_type()?.is_symlink())
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
                // Reject Win32 wildcards and forbidden characters
                if s.chars()
                    .any(|c| matches!(c, '*' | '?' | '<' | '>' | '|' | '"'))
                {
                    return false;
                }
                // Reject components with trailing spaces or dots (Windows strips these)
                if s.ends_with(' ') || s.ends_with('.') {
                    return false;
                }
                // Normalize Unicode superscripts ('⁰'..'⁹') before stem extraction
                let s = s
                    .replace('⁰', "0")
                    .replace('¹', "1")
                    .replace('²', "2")
                    .replace('³', "3")
                    .replace('⁴', "4")
                    .replace('⁵', "5")
                    .replace('⁶', "6")
                    .replace('⁷', "7")
                    .replace('⁸', "8")
                    .replace('⁹', "9");
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
    fn test_path_safety_superscripts_and_wildcards() {
        assert!(!is_safe_relative_path(Path::new("COM¹")));
        assert!(!is_safe_relative_path(Path::new("LPT²")));
        assert!(!is_safe_relative_path(Path::new("file*.txt")));
        assert!(!is_safe_relative_path(Path::new("file?.bin")));
        assert!(!is_safe_relative_path(Path::new("file<tag>.txt")));
        assert!(!is_safe_relative_path(Path::new("file|pipe.txt")));
        assert!(is_safe_relative_path(Path::new("normal_file.txt")));
    }

    #[test]
    fn test_path_safety_dest_dir_root_reparse() {
        let tmp = tempfile::tempdir().unwrap();
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir(&dest_dir).unwrap();
        // Non-existent relative subpath with valid dest_dir
        let res = verify_destination_not_reparse(&dest_dir, Path::new("sub/file.txt"));
        assert!(res.is_ok());
    }

    #[test]
    fn test_verify_destination_caches_root() {
        let tmp = tempfile::tempdir().unwrap();
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir(&dest_dir).unwrap();

        let mut verified = HashSet::new();
        let file_rel = Path::new("sub/file.txt");
        assert!(!verified.contains(&dest_dir));

        let res = verify_destination_not_reparse_cached(&dest_dir, file_rel, &mut verified);
        assert!(res.is_ok());
        assert!(
            verified.contains(&dest_dir),
            "Expected dest_dir to be cached in verified_dirs"
        );
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

    #[test]
    fn test_verify_destination_not_reparse_cached_no_io_on_cached_ancestors() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        let deep = dest.join("cached_ancestor");
        fs::create_dir_all(&deep).unwrap();
        let file = deep.join("leaf.txt");
        fs::write(&file, b"content").unwrap();

        let mut cache = HashSet::new();
        // Pre-insert deep into cache
        cache.insert(deep.clone());

        let meta = verify_destination_not_reparse_cached(
            &dest,
            Path::new("cached_ancestor/leaf.txt"),
            &mut cache,
        )
        .unwrap();

        assert!(meta.is_some());
        let meta = meta.unwrap();
        assert!(
            meta.is_file(),
            "leaf metadata must be for the leaf file, not an ancestor"
        );
    }

    #[test]
    fn test_verify_destination_not_reparse_cached_leaf_in_cache_returns_metadata() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        let dir = dest.join("cached_dir");
        fs::create_dir_all(&dir).unwrap();

        let mut cache = HashSet::new();
        cache.insert(dir.clone());

        let meta =
            verify_destination_not_reparse_cached(&dest, Path::new("cached_dir"), &mut cache)
                .unwrap();

        assert!(meta.is_some());
        let meta = meta.unwrap();
        assert!(
            meta.is_dir(),
            "cached leaf directory must return directory metadata"
        );
    }

    #[test]
    fn test_is_reparse_or_symlink_regular_dir_is_false() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        for entry in fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let result = is_reparse_or_symlink(&entry).unwrap();
            assert!(
                !result,
                "regular directory must not be flagged as reparse point"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_is_reparse_or_symlink_detects_junction_or_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::create_dir(&target).unwrap();

        // Try creating symlink or junction
        let created = if std::os::windows::fs::symlink_dir(&target, &link).is_ok() {
            true
        } else {
            let status = std::process::Command::new("powershell")
                .args([
                    "-Command",
                    &format!(
                        "New-Item -ItemType Junction -Path '{}' -Target '{}' -Force",
                        link.display(),
                        target.display()
                    ),
                ])
                .status();
            status.map(|s| s.success()).unwrap_or(false)
        };

        if created {
            for entry in fs::read_dir(dir.path()).unwrap() {
                let entry = entry.unwrap();
                if entry.path() == link {
                    let result = is_reparse_or_symlink(&entry).unwrap();
                    assert!(result, "junction/symlink must be detected as reparse point");
                }
            }
        }
    }
}

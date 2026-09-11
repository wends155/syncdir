use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::error::SyncError;

/// Two-tiered reparse point verification cache partitioned by relative depth from the sync root.
///
/// Ancestors within 3 levels of the base root are retained in a persistent `shallow` set (up to `max_shallow` entries)
/// to eliminate redundant network reparse validation on SMB roots. Deep directories beyond depth 3 are kept in `deep`
/// (up to `max_deep` entries) with automatic eviction upon capacity breach.
#[derive(Debug)]
pub struct ReparseCache {
    inner: RwLock<ReparseCacheInner>,
    max_shallow: usize,
    max_deep: usize,
}

#[derive(Debug)]
struct ReparseCacheInner {
    shallow: HashSet<PathBuf>,
    deep: HashSet<PathBuf>,
}

impl ReparseCache {
    /// Create a new `ReparseCache` with given shallow and deep capacity limits.
    pub fn new(max_shallow: usize, max_deep: usize) -> Self {
        Self {
            inner: RwLock::new(ReparseCacheInner {
                shallow: HashSet::new(),
                deep: HashSet::new(),
            }),
            max_shallow,
            max_deep,
        }
    }

    /// Check whether `path` is contained in either the shallow or deep cache.
    pub fn contains(&self, path: &Path) -> bool {
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.shallow.contains(path) || inner.deep.contains(path)
    }

    /// Insert a verified ancestor directory, classifying it into `shallow` (relative depth <= 3) or `deep`.
    pub fn insert_ancestor(&self, base_root: &Path, ancestor: &Path) {
        let rel_depth = match ancestor.strip_prefix(base_root) {
            Ok(rel) => rel.components().count(),
            Err(_) => 0,
        };
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if rel_depth <= 3 {
            if inner.shallow.len() >= self.max_shallow {
                inner.shallow.clear();
            }
            inner.shallow.insert(ancestor.to_path_buf());
        } else {
            if inner.deep.len() >= self.max_deep {
                inner.deep.clear();
            }
            inner.deep.insert(ancestor.to_path_buf());
        }
    }

    /// Evict `dir` and all its descendants from both cache tiers.
    pub fn evict_dir(&self, dir: &Path) {
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.shallow.retain(|p| !p.starts_with(dir));
        inner.deep.retain(|p| !p.starts_with(dir));
    }

    /// Clear all cached directories across both tiers.
    pub fn clear(&self) {
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.shallow.clear();
        inner.deep.clear();
    }

    /// Number of entries in the shallow cache tier.
    pub fn shallow_len(&self) -> usize {
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.shallow.len()
    }

    /// Number of entries in the deep cache tier.
    pub fn deep_len(&self) -> usize {
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.deep.len()
    }

    /// Total number of cached entries across both tiers.
    pub fn len(&self) -> usize {
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.shallow.len() + inner.deep.len()
    }

    /// Returns `true` if no entries are cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(windows)]
pub(crate) fn verify_destination_not_reparse(
    dest_dir: &Path,
    rel_path: &Path,
) -> Result<Option<Metadata>, SyncError> {
    if fs::symlink_metadata(dest_dir)
        .map(|m| is_reparse_or_symlink_meta(&m))
        .unwrap_or(false)
    {
        return Err(SyncError::validation_reparse(format!(
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
                return Err(SyncError::validation_reparse(format!(
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
        return Err(SyncError::validation_reparse(format!(
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
                return Err(SyncError::validation_reparse(format!(
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
pub fn verify_destination_not_reparse_cached(
    dest_dir: &Path,
    rel_path: &Path,
    cache: &ReparseCache,
) -> Result<Option<Metadata>, SyncError> {
    if !cache.contains(dest_dir) {
        if fs::symlink_metadata(dest_dir)
            .map(|m| is_reparse_or_symlink_meta(&m))
            .unwrap_or(false)
        {
            return Err(SyncError::validation_reparse(format!(
                "Destination root '{}' is a symlink or reparse point; refusing to write",
                dest_dir.display()
            )));
        }
        cache.insert_ancestor(dest_dir, dest_dir);
    }
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    let components: Vec<_> = rel_path.components().collect();
    let total = components.len();
    for (i, component) in components.into_iter().enumerate() {
        let is_leaf = i + 1 == total;
        current.push(component);
        if cache.contains(&current) {
            if is_leaf {
                leaf_meta = fs::symlink_metadata(&current).ok();
            }
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation_reparse(format!(
                    "Destination component '{}' is a symlink or reparse point; refusing to write",
                    current.display()
                )));
            }
            if m.is_dir() {
                cache.insert_ancestor(dest_dir, &current);
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
pub fn verify_destination_not_reparse_cached(
    dest_dir: &Path,
    rel_path: &Path,
    cache: &ReparseCache,
) -> Result<Option<Metadata>, SyncError> {
    if !cache.contains(dest_dir) {
        if let Ok(m) = fs::symlink_metadata(dest_dir) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation_reparse(format!(
                    "Destination root '{}' is a symlink; refusing to write",
                    dest_dir.display()
                )));
            }
        }
        cache.insert_ancestor(dest_dir, dest_dir);
    }
    let mut current = dest_dir.to_path_buf();
    let mut leaf_meta = None;
    let components: Vec<_> = rel_path.components().collect();
    let total = components.len();
    for (i, component) in components.into_iter().enumerate() {
        let is_leaf = i + 1 == total;
        current.push(component);
        if cache.contains(&current) {
            if is_leaf {
                leaf_meta = fs::symlink_metadata(&current).ok();
            }
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation_reparse(format!(
                    "Destination component '{}' is a symlink; refusing to write",
                    current.display()
                )));
            }
            if m.is_dir() {
                cache.insert_ancestor(dest_dir, &current);
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
pub fn verify_source_not_reparse_cached(
    source_dir: &Path,
    rel_path: &Path,
    cache: &ReparseCache,
) -> Result<(), SyncError> {
    if !cache.contains(source_dir) {
        if fs::symlink_metadata(source_dir)
            .map(|m| is_reparse_or_symlink_meta(&m))
            .unwrap_or(false)
        {
            return Err(SyncError::validation_reparse(format!(
                "Source root '{}' is a symlink or reparse point; refusing to read",
                source_dir.display()
            )));
        }
        cache.insert_ancestor(source_dir, source_dir);
    }
    let mut current = source_dir.to_path_buf();
    for component in rel_path.parent().into_iter().flat_map(|p| p.components()) {
        current.push(component);
        if cache.contains(&current) {
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation_reparse(format!(
                    "Source ancestor '{}' is a symlink or reparse point; refusing to read",
                    current.display()
                )));
            }
            if m.is_dir() {
                cache.insert_ancestor(source_dir, &current);
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn verify_source_not_reparse_cached(
    source_dir: &Path,
    rel_path: &Path,
    cache: &ReparseCache,
) -> Result<(), SyncError> {
    if !cache.contains(source_dir) {
        if let Ok(m) = fs::symlink_metadata(source_dir) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation_reparse(format!(
                    "Source root '{}' is a symlink; refusing to read",
                    source_dir.display()
                )));
            }
        }
        cache.insert_ancestor(source_dir, source_dir);
    }
    let mut current = source_dir.to_path_buf();
    for component in rel_path.parent().into_iter().flat_map(|p| p.components()) {
        current.push(component);
        if cache.contains(&current) {
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&current) {
            if is_reparse_or_symlink_meta(&m) {
                return Err(SyncError::validation_reparse(format!(
                    "Source ancestor '{}' is a symlink; refusing to read",
                    current.display()
                )));
            }
            if m.is_dir() {
                cache.insert_ancestor(source_dir, &current);
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
#[allow(dead_code)]
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
            return Err(SyncError::validation_reparse(format!(
                "Source ancestor '{}' is a symlink or reparse point; refusing to read",
                current.display()
            )));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
#[allow(dead_code)]
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
            return Err(SyncError::validation_reparse(format!(
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

        let cache = ReparseCache::new(50_000, 10_000);
        let file_rel = Path::new("sub/file.txt");
        assert!(!cache.contains(&dest_dir));

        let res = verify_destination_not_reparse_cached(&dest_dir, file_rel, &cache);
        assert!(res.is_ok());
        assert!(
            cache.contains(&dest_dir),
            "Expected dest_dir to be cached in reparse cache"
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

        let cache = ReparseCache::new(50_000, 10_000);
        let meta1 =
            verify_destination_not_reparse_cached(&dest, Path::new("a/b/c/file1.txt"), &cache)
                .unwrap();
        assert!(meta1.is_some());
        assert!(!cache.is_empty());
        let count_before = cache.len();

        // Second file in same directory should hit cache for all ancestors
        let meta2 =
            verify_destination_not_reparse_cached(&dest, Path::new("a/b/c/file2.txt"), &cache)
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

        let cache = ReparseCache::new(50_000, 10_000);
        // Pre-insert deep into cache
        cache.insert_ancestor(&dest, &deep);

        let meta = verify_destination_not_reparse_cached(
            &dest,
            Path::new("cached_ancestor/leaf.txt"),
            &cache,
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

        let cache = ReparseCache::new(50_000, 10_000);
        cache.insert_ancestor(&dest, &dir);

        let meta =
            verify_destination_not_reparse_cached(&dest, Path::new("cached_dir"), &cache).unwrap();

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

    #[cfg(windows)]
    #[test]
    fn test_verify_destination_and_source_reparse_error_classification() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("target_dir");
        let link = temp.path().join("link_dir");
        fs::create_dir_all(&target).unwrap();

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
            // Test verify_destination_not_reparse on root junction
            let res_dest_root = verify_destination_not_reparse(&link, Path::new("sub/file.txt"));
            assert!(res_dest_root.is_err());
            let err = res_dest_root.err().unwrap();
            assert!(
                err.is_permanent_validation_failure(),
                "Reparse error must be permanent validation failure"
            );
            assert!(matches!(
                err,
                SyncError::Validation {
                    kind: crate::error::ValidationKind::ReparsePoint,
                    ..
                }
            ));

            // Test verify_destination_not_reparse on component junction
            let valid_dest = temp.path().join("valid_dest");
            fs::create_dir_all(&valid_dest).unwrap();
            let dest_comp_link = valid_dest.join("junction_comp");
            let _ = std::process::Command::new("powershell")
                .args([
                    "-Command",
                    &format!(
                        "New-Item -ItemType Junction -Path '{}' -Target '{}' -Force",
                        dest_comp_link.display(),
                        target.display()
                    ),
                ])
                .status();
            if dest_comp_link.exists() {
                let res_dest_comp = verify_destination_not_reparse(
                    &valid_dest,
                    Path::new("junction_comp/file.txt"),
                );
                assert!(res_dest_comp.is_err());
                let err_comp = res_dest_comp.err().unwrap();
                assert!(
                    err_comp.is_permanent_validation_failure(),
                    "Reparse component must be permanent validation failure"
                );
                assert!(matches!(
                    err_comp,
                    SyncError::Validation {
                        kind: crate::error::ValidationKind::ReparsePoint,
                        ..
                    }
                ));
            }

            // Test verify_source_not_reparse on component junction
            let valid_src = temp.path().join("valid_src");
            fs::create_dir_all(&valid_src).unwrap();
            let src_comp_link = valid_src.join("src_junction");
            let _ = std::process::Command::new("powershell")
                .args([
                    "-Command",
                    &format!(
                        "New-Item -ItemType Junction -Path '{}' -Target '{}' -Force",
                        src_comp_link.display(),
                        target.display()
                    ),
                ])
                .status();
            if src_comp_link.exists() {
                let res_src =
                    verify_source_not_reparse(&valid_src, Path::new("src_junction/file.txt"));
                assert!(res_src.is_err());
                let err_src = res_src.err().unwrap();
                assert!(
                    err_src.is_permanent_validation_failure(),
                    "Source reparse component must be permanent validation failure"
                );
                assert!(matches!(
                    err_src,
                    SyncError::Validation {
                        kind: crate::error::ValidationKind::ReparsePoint,
                        ..
                    }
                ));
            }
        }
    }

    #[test]
    fn test_reparse_cache_shallow_retained_and_deep_evicted() {
        let cache = ReparseCache::new(50_000, 10_000);
        let root = Path::new("C:/dest");
        let shallow = root.join("a/b/c"); // depth 3 from root
        let deep = root.join("a/b/c/d/e"); // depth 5 from root

        cache.insert_ancestor(root, &shallow);
        cache.insert_ancestor(root, &deep);

        assert_eq!(cache.shallow_len(), 1);
        assert_eq!(cache.deep_len(), 1);
        assert!(cache.contains(&shallow));
        assert!(cache.contains(&deep));
    }

    #[test]
    fn test_reparse_cache_eviction_bounded_capacity() {
        let cache = ReparseCache::new(2, 2); // max 2 shallow, 2 deep
        let root = Path::new("C:/dest");
        let d1 = root.join("a/b/c/d/1");
        let d2 = root.join("a/b/c/d/2");
        cache.insert_ancestor(root, &d1);
        cache.insert_ancestor(root, &d2);
        assert_eq!(cache.deep_len(), 2);
        assert!(cache.contains(&d1));
        assert!(cache.contains(&d2));

        let d3 = root.join("a/b/c/d/3");
        cache.insert_ancestor(root, &d3);
        assert_eq!(cache.deep_len(), 1);
        assert!(cache.contains(&d3));
        assert!(!cache.contains(&d1), "Older deep entry d1 must be evicted");
        assert!(!cache.contains(&d2), "Older deep entry d2 must be evicted");
    }

    #[test]
    fn test_normalize_superscripts_cow_zero_allocation_on_ascii() {
        let ascii = "plain_path/file.txt";
        let norm = normalize_superscripts_cow(ascii);
        assert!(matches!(norm, std::borrow::Cow::Borrowed(_)));
        assert_eq!(norm, "plain_path/file.txt");

        let superscript = "COM¹";
        let norm2 = normalize_superscripts_cow(superscript);
        assert!(matches!(norm2, std::borrow::Cow::Owned(_)));
        assert_eq!(norm2, "COM1");
    }

    #[test]
    fn test_verify_destination_not_reparse_cached_with_reparse_cache() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        let deep = dest.join("sub").join("nested");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("test.txt"), b"data").unwrap();

        let cache = ReparseCache::new(50_000, 10_000);
        let meta =
            verify_destination_not_reparse_cached(&dest, Path::new("sub/nested/test.txt"), &cache)
                .unwrap();
        assert!(meta.is_some());
        assert!(cache.contains(&dest));
        assert!(cache.contains(&dest.join("sub")));
        assert!(cache.contains(&deep));

        // Verify source cached helper
        assert!(
            verify_source_not_reparse_cached(&dest, Path::new("sub/nested/test.txt"), &cache)
                .is_ok()
        );
    }
}

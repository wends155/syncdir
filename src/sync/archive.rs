use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::path_safety::{
    ReparseCache, is_reparse_or_symlink, is_reparse_or_symlink_meta, is_safe_relative_path,
    verify_destination_not_reparse_cached,
};
use crate::config::TargetSyncConfig;
use crate::error::SyncError;

/// Global atomic counter for unique archive path generation across threads and timestamps.
static ARCHIVE_NONCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn parse_archive_timestamp(name: &str) -> Option<SystemTime> {
    let prefix = name.split('_').next()?;
    let millis: u64 = prefix.parse().ok()?;
    if millis >= 1_000_000_000_000 {
        Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(millis))
    } else {
        None
    }
}

/// Maximum candidate files evaluated per archive pruning cycle to bound memory and I/O.
pub(crate) const MAX_ARCHIVE_PRUNE_CANDIDATES: usize = 5000;

pub(crate) fn prune_archive(
    archive_dir: &Path,
    max_age_days: u64,
    max_bytes: u64,
) -> Result<(), SyncError> {
    let sym_meta = match fs::symlink_metadata(archive_dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(SyncError::Io(e)),
    };
    if is_reparse_or_symlink_meta(&sym_meta) || !sym_meta.is_dir() {
        return Err(SyncError::validation_reparse(format!(
            "Archive directory '{}' is a reparse point or junction; refusing to prune",
            archive_dir.display()
        )));
    }
    let max_age = std::time::Duration::from_secs(max_age_days.saturating_mul(86_400));
    let now = SystemTime::now();

    let mut files = Vec::new();
    let mut total_bytes = 0u64;

    fn collect_files(
        dir: &Path,
        files: &mut Vec<(PathBuf, u64, SystemTime)>,
        total_bytes: &mut u64,
        inherited_time: Option<SystemTime>,
        depth: usize,
    ) -> std::io::Result<()> {
        const MAX_ARCHIVE_DEPTH: usize = 32;
        if depth > MAX_ARCHIVE_DEPTH || files.len() >= MAX_ARCHIVE_PRUNE_CANDIDATES {
            if depth > MAX_ARCHIVE_DEPTH {
                tracing::warn!(path = %dir.display(), "Max archive directory depth exceeded, skipping");
            }
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            if files.len() >= MAX_ARCHIVE_PRUNE_CANDIDATES {
                break;
            }
            let entry = entry?;
            match is_reparse_or_symlink(&entry) {
                Ok(true) => {
                    tracing::debug!(path = %entry.path().display(), "Skipping reparse point or symlink in archive");
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(path = %entry.path().display(), error = %e, "Failed checking reparse point; skipping");
                    continue;
                }
            }
            let ft = entry.file_type()?;
            let path = entry.path();
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            let parsed_time = parse_archive_timestamp(&name_str);

            if ft.is_dir() {
                let next_inherited = parsed_time.or(inherited_time);
                collect_files(&path, files, total_bytes, next_inherited, depth + 1)?;
                if files.len() >= MAX_ARCHIVE_PRUNE_CANDIDATES {
                    break;
                }
            } else if ft.is_file()
                && let Ok(meta) = entry.metadata()
            {
                let len = meta.len();
                let archive_time = parsed_time
                    .or(inherited_time)
                    .or_else(|| meta.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                *total_bytes += len;
                files.push((path, len, archive_time));
            }
        }
        Ok(())
    }

    let _ = collect_files(archive_dir, &mut files, &mut total_bytes, None, 0);

    // Evict files older than max_age_days based on archive entry time
    files.retain(|(path, len, archive_time)| {
        if let Ok(age) = now.duration_since(*archive_time)
            && age > max_age
            && fs::remove_file(path).is_ok()
        {
            total_bytes = total_bytes.saturating_sub(*len);
            return false;
        }
        true
    });

    // If total_bytes still exceeds max_bytes, evict oldest first
    if total_bytes > max_bytes {
        files.sort_by_key(|(_, _, m)| *m);
        for (path, len, _) in files {
            if total_bytes <= max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total_bytes = total_bytes.saturating_sub(len);
            }
        }
    }

    Ok(())
}

/// Dedicated collaborating manager for timestamped archive file creation, path resolution, and retention pruning.
#[derive(Debug, Clone)]
pub(crate) struct ArchiveManager {
    config: TargetSyncConfig,
    reparse_cache: Arc<ReparseCache>,
}

impl ArchiveManager {
    pub(crate) fn new(config: TargetSyncConfig, reparse_cache: Arc<ReparseCache>) -> Self {
        Self {
            config,
            reparse_cache,
        }
    }

    /// Build the archive path: `<dest>/.syncdir_archive/<ts>_<relative_path>`.
    pub(crate) fn get_archive_path(
        &self,
        dest_dir: &Path,
        relative_path: &Path,
        timestamp: &str,
    ) -> Result<PathBuf, SyncError> {
        let mut components = relative_path.components();
        if let Some(first) = components.next() {
            let first_str = first.as_os_str().to_string_lossy();
            let prefixed = format!("{}_{}", timestamp, first_str);
            let mut archive_rel = PathBuf::from(prefixed);
            for rest in components {
                archive_rel.push(rest);
            }
            Ok(dest_dir.join(".syncdir_archive").join(archive_rel))
        } else {
            Ok(dest_dir.join(".syncdir_archive"))
        }
    }

    /// Archive or remove a file on destination filesystem without updating the database.
    pub(crate) fn archive_dest_file_only(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
    ) -> Result<(), SyncError> {
        if !is_safe_relative_path(rel_path) {
            return Err(SyncError::validation_security(format!(
                "Unsafe path traversal detected: {}",
                rel_path.display()
            )));
        }
        let dest_path = dest_dir.join(rel_path);

        if !dest_dir.exists() {
            return Err(SyncError::Io(std::io::Error::from_raw_os_error(53)));
        }

        // Verify destination path does not traverse reparse points
        verify_destination_not_reparse_cached(dest_dir, rel_path, &self.reparse_cache)?;

        match fs::metadata(&dest_path) {
            Ok(_) => {
                if self.config.propagate_deletions() {
                    let archive_dir = dest_dir.join(".syncdir_archive");
                    verify_destination_not_reparse_cached(
                        &archive_dir,
                        Path::new(""),
                        &self.reparse_cache,
                    )?;

                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|e| {
                            SyncError::Io(std::io::Error::other(format!("System clock error: {e}")))
                        })?
                        .as_millis();

                    let nonce =
                        ARCHIVE_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 10_000;
                    let token = format!("{timestamp}_{nonce:04}");

                    let mut archive_path = self.get_archive_path(dest_dir, rel_path, &token)?;
                    let mut counter = 1u32;
                    while archive_path.exists() {
                        archive_path = self.get_archive_path(
                            dest_dir,
                            rel_path,
                            &format!("{token}_{counter}"),
                        )?;
                        counter += 1;
                    }

                    let archive_rel = archive_path
                        .strip_prefix(&archive_dir)
                        .map_err(|e| SyncError::validation_security(e.to_string()))?;
                    verify_destination_not_reparse_cached(
                        &archive_dir,
                        archive_rel,
                        &self.reparse_cache,
                    )?;

                    if let Some(parent) = archive_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    verify_destination_not_reparse_cached(
                        &archive_dir,
                        archive_rel,
                        &self.reparse_cache,
                    )?;
                    fs::rename(&dest_path, &archive_path)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(SyncError::Io(e));
            }
        }
        Ok(())
    }

    /// Prune old and excess files in the destination archive.
    pub(crate) fn prune_destination_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        let archive_dir = dest_dir.join(".syncdir_archive");
        prune_archive(&archive_dir, 30, 10 * 1024 * 1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::super::path_safety::verify_destination_not_reparse;
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use tempfile::tempdir;

    fn test_config(source: std::path::PathBuf, dest: std::path::PathBuf) -> Config {
        Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(10)
            .block_size_bytes(4)
            .build()
            .unwrap()
    }

    #[cfg(windows)]
    fn create_test_junction(target: &Path, link: &Path) -> std::io::Result<()> {
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            let status = std::process::Command::new("powershell")
                .args([
                    "-Command",
                    &format!(
                        "New-Item -ItemType Junction -Path '{}' -Target '{}' -Force",
                        link.display(),
                        target.display()
                    ),
                ])
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other("Failed to create test junction"));
            }
        }
        Ok(())
    }

    #[test]
    fn test_prune_archive_retention() {
        let temp = tempdir().unwrap();
        let archive = temp.path().join(".syncdir_archive");
        std::fs::create_dir_all(&archive).unwrap();
        let f1 = archive.join("old.txt");
        let f2 = archive.join("new.txt");
        std::fs::write(&f1, vec![0u8; 100]).unwrap();
        std::fs::write(&f2, vec![0u8; 200]).unwrap();

        // Prune with max_bytes = 150
        prune_archive(&archive, 365, 150).unwrap();
        let total_remaining: u64 = std::fs::read_dir(&archive)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total_remaining <= 150);
    }

    #[test]
    fn test_prune_archive_depth_limit_32() {
        let temp = tempdir().unwrap();
        let archive = temp.path().join(".syncdir_archive");
        let mut deep = archive.clone();
        for i in 0..35 {
            deep = deep.join(format!("level_{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        let deep_file = deep.join("deep.txt");
        std::fs::write(&deep_file, vec![0u8; 1000]).unwrap();

        prune_archive(&archive, 365, 100).unwrap();
        assert!(deep_file.exists());
    }

    #[cfg(windows)]
    #[test]
    fn test_prune_archive_ignores_symlinks_and_junctions() {
        let temp = tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("important.txt");
        std::fs::write(&outside_file, vec![0u8; 500]).unwrap();

        let archive = temp.path().join(".syncdir_archive");
        std::fs::create_dir_all(&archive).unwrap();
        let link = archive.join("junction_link");

        if create_test_junction(&outside, &link).is_ok() {
            prune_archive(&archive, 365, 100).unwrap();
            assert!(outside_file.exists());
        }
    }

    #[test]
    fn test_archive_path_nonce_uniqueness_concurrent() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let reparse_cache = Arc::new(ReparseCache::new(1000, 100));
        let archive_manager = Arc::new(ArchiveManager::new(target_cfg, reparse_cache));

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(50));
        let mut handles = Vec::new();

        for i in 0..50 {
            let archive_manager = archive_manager.clone();
            let barrier = barrier.clone();
            let dst = dst.clone();
            handles.push(std::thread::spawn(move || {
                let file_name = format!("file_{i}.txt");
                let file_path = dst.join(&file_name);
                std::fs::write(&file_path, format!("content {i}")).unwrap();
                barrier.wait();
                archive_manager
                    .archive_dest_file_only(Path::new(&file_name), &dst)
                    .unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let archive_dir = dst.join(".syncdir_archive");
        let archived_entries: Vec<_> = std::fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        assert_eq!(
            archived_entries.len(),
            50,
            "All 50 files must be archived into distinct files without collisions"
        );

        for entry in archived_entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let parts: Vec<&str> = name.split('_').collect();
            assert!(
                parts.len() >= 3,
                "Archive filename '{name}' must have format '<ts>_<nonce>_<filename>'"
            );
            assert_eq!(
                parts[1].len(),
                4,
                "Nonce component '{}' must be 4 digits",
                parts[1]
            );
            assert!(
                parts[1].chars().all(|c| c.is_ascii_digit()),
                "Nonce component '{}' must be numeric",
                parts[1]
            );
        }
    }

    #[test]
    fn test_prune_archive_preserves_newly_archived_old_files() {
        use std::time::UNIX_EPOCH;
        let temp = tempfile::tempdir().unwrap();
        let archive_dir = temp.path().join(".syncdir_archive");
        std::fs::create_dir_all(&archive_dir).unwrap();

        let now_millis = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let file_name = format!("{}_{}_test_old_file.txt", now_millis, "0001");
        let file_path = archive_dir.join(&file_name);
        std::fs::write(&file_path, b"content").unwrap();

        // Set creation and modification time to 60 days ago
        let sixty_days_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 86400);
        let f = std::fs::File::options()
            .write(true)
            .open(&file_path)
            .unwrap();
        let mut times = std::fs::FileTimes::new().set_modified(sixty_days_ago);
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTimesExt;
            times = times.set_created(sixty_days_ago);
        }
        f.set_times(times).unwrap();
        drop(f);

        // Prune with 30-day max age
        prune_archive(&archive_dir, 30, 10_000_000).unwrap();

        // File MUST still exist because filename timestamp is recent
        assert!(
            file_path.exists(),
            "Newly archived file with old btime was incorrectly pruned"
        );
    }

    #[test]
    fn test_prune_archive_retains_newly_archived_old_file() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let dir = tempdir().unwrap();
        let archive_dir = dir.path().join(".syncdir_archive");
        std::fs::create_dir_all(&archive_dir).unwrap();

        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let file_path = archive_dir.join(format!("{}_0001_old_content_file.txt", now_millis));
        std::fs::write(&file_path, b"important backup").unwrap();

        // Set mtime to 60 days ago
        let sixty_days_ago = SystemTime::now() - Duration::from_secs(60 * 24 * 3600);
        if let Ok(file) = std::fs::File::options().write(true).open(&file_path) {
            let times = std::fs::FileTimes::new().set_modified(sixty_days_ago);
            let _ = file.set_times(times);
        }

        // Newly archived file (created now) must be retained even though mtime is 60 days old
        prune_archive(&archive_dir, 30, u64::MAX).unwrap();
        assert!(
            file_path.exists(),
            "newly archived file must be retained regardless of payload mtime"
        );
    }

    #[test]
    fn test_prune_archive_nested_directory_timestamp_inheritance() {
        use std::time::UNIX_EPOCH;
        let temp = tempfile::tempdir().unwrap();
        let archive_dir = temp.path().join(".syncdir_archive");
        let now_millis = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let top_dir = archive_dir.join(format!("{}_{}_nested_folder", now_millis, "0002"));
        let sub_dir = top_dir.join("subdir");
        std::fs::create_dir_all(&sub_dir).unwrap();

        let nested_file = sub_dir.join("deep_file.txt");
        std::fs::write(&nested_file, b"deep content").unwrap();

        let sixty_days_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 86400);
        let f = std::fs::File::options()
            .write(true)
            .open(&nested_file)
            .unwrap();
        let mut times = std::fs::FileTimes::new().set_modified(sixty_days_ago);
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTimesExt;
            times = times.set_created(sixty_days_ago);
        }
        f.set_times(times).unwrap();
        drop(f);

        prune_archive(&archive_dir, 30, 10_000_000).unwrap();
        assert!(
            nested_file.exists(),
            "Nested file inheriting recent parent timestamp was incorrectly pruned"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_archive_dest_file_reparse_rejection() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let archive_junction = dst.join(".syncdir_archive");
        if create_test_junction(&outside, &archive_junction).is_err() {
            return;
        }

        let config = Config::builder(src)
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let reparse_cache = Arc::new(ReparseCache::new(1000, 100));
        let archive_manager = ArchiveManager::new(target_cfg, reparse_cache);

        let file = dst.join("victim.txt");
        std::fs::write(&file, "payload").unwrap();

        let res = archive_manager.archive_dest_file_only(Path::new("victim.txt"), &dst);
        assert!(
            res.is_err(),
            "Must reject archiving when .syncdir_archive is a reparse point"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_prune_archive_returns_err_on_junction_archive_dir() {
        let dir = tempdir().unwrap();
        let outside = dir.path().join("outside");
        let archive_junction = dir.path().join("archive_junction");
        std::fs::create_dir_all(&outside).unwrap();

        if create_test_junction(&outside, &archive_junction).is_err() {
            return;
        }

        let res = prune_archive(&archive_junction, 30, 10_000_000);
        assert!(
            res.is_err(),
            "prune_archive must reject junction archive_dir"
        );
        let err = res.unwrap_err();
        assert!(
            matches!(err, SyncError::Validation { .. }),
            "Expected Validation error, got {:?}",
            err
        );
        assert!(err.to_string().contains("reparse point or junction"));
    }

    #[test]
    fn test_archive_manager_standalone() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let cfg = test_config(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::from_config(&cfg, dest.clone()).unwrap();
        let reparse_cache = Arc::new(ReparseCache::new(1000, 100));
        let manager = ArchiveManager::new(target_cfg, reparse_cache);

        let p = manager
            .get_archive_path(&dest, Path::new("sub/doc.txt"), "20260910")
            .unwrap();
        assert_eq!(
            p,
            dest.join(".syncdir_archive")
                .join("20260910_sub")
                .join("doc.txt")
        );

        let file = dest.join("victim.txt");
        fs::write(&file, b"content").unwrap();
        manager
            .archive_dest_file_only(Path::new("victim.txt"), &dest)
            .unwrap();
        assert!(!file.exists(), "Original file should be moved to archive");

        let archive_dir = dest.join(".syncdir_archive");
        assert!(archive_dir.exists());

        let prune_res = manager.prune_destination_archive(&dest);
        assert!(prune_res.is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn test_archive_dest_file_intermediate_subpath_ancestor_reparse_validation() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let archive_dir = dst.join(".syncdir_archive");
        fs::create_dir_all(&archive_dir).unwrap();

        let intermediate_junction = archive_dir.join("sub_junction");
        if create_test_junction(&outside, &intermediate_junction).is_err() {
            return;
        }

        let res =
            verify_destination_not_reparse(&archive_dir, Path::new("sub_junction/nested/file.txt"));
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.is_permanent_validation_failure());
        assert!(matches!(
            err,
            SyncError::Validation {
                kind: crate::error::ValidationKind::ReparsePoint,
                ..
            }
        ));
    }

    #[test]
    fn test_archive_manager_prune_bounded_candidate_limit() {
        assert_eq!(MAX_ARCHIVE_PRUNE_CANDIDATES, 5000);
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        let archive_dir = dest.join(".syncdir_archive");
        fs::create_dir_all(&archive_dir).unwrap();

        // Create 10 dummy archive files
        for i in 0..10 {
            let f = archive_dir.join(format!("172600000000{}_file.txt", i));
            fs::write(f, b"dummy").unwrap();
        }

        // Direct prune with 0 age days and 0 max bytes enforces eviction ceiling
        assert!(prune_archive(&archive_dir, 0, 0).is_ok());
        let remaining = fs::read_dir(&archive_dir).unwrap().count();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn test_archive_dest_file_only_queries_reparse_cache() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let cfg = test_config(source, dest.clone());
        let target_cfg = TargetSyncConfig::from_config(&cfg, dest.clone()).unwrap();
        let reparse_cache = Arc::new(ReparseCache::new(1000, 100));
        assert!(reparse_cache.is_empty());

        let manager = ArchiveManager::new(target_cfg, Arc::clone(&reparse_cache));

        let file = dest.join("victim.txt");
        fs::write(&file, b"content").unwrap();
        manager
            .archive_dest_file_only(Path::new("victim.txt"), &dest)
            .unwrap();

        assert!(
            !reparse_cache.is_empty(),
            "ReparseCache must be populated during archive_dest_file_only"
        );
        assert!(
            reparse_cache.contains(&dest),
            "ReparseCache must contain destination root"
        );
    }
}

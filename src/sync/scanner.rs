use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::TargetSyncConfig;
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;

use super::engine::{LocalSyncEngine, ScanOutcome};
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
        tracing::warn!(path = %dir.display(), "Max directory depth exceeded, skipping");
        *scan_complete = false;
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!(path = %dir.display(), error = %e, "Permission denied scanning directory; skipping");
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
        match is_reparse_or_symlink(&entry) {
            Ok(true) => {
                tracing::debug!(path = %entry.path().display(), "Skipping reparse point or symlink in scan");
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(path = %entry.path().display(), error = %e, "Failed checking reparse point; skipping entry and marking scan incomplete");
                *scan_complete = false;
                continue;
            }
        }
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::warn!(path = %entry.path().display(), error = %e, "Permission denied querying file type; skipping");
                *scan_complete = false;
                continue;
            }
            Err(e) => return Err(SyncError::Io(e)),
        };
        let path = entry.path();
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

impl<S: HashStore> LocalSyncEngine<S> {
    /// Flush accumulated file records and hashes to the database in a single batch.
    pub(crate) fn flush_record_batch(
        &self,
        batch: &mut Vec<(FileRecord, Vec<crate::db::BlockHash>)>,
    ) -> Result<(), SyncError> {
        if batch.is_empty() {
            return Ok(());
        }
        let refs: Vec<(&FileRecord, &[crate::db::BlockHash])> = batch
            .iter()
            .map(|(rec, hashes)| (rec, hashes.as_slice()))
            .collect();
        self.db.save_files_batch(&refs)?;
        batch.clear();
        Ok(())
    }

    /// Full scan implementation with cooperative cancellation support.
    pub(crate) fn run_cancellable_full_scan_impl(
        &self,
        dest_dir: &Path,
        cancel: &AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }

        let resolved_source = self.config.source_dir();
        if !resolved_source.exists() {
            return Err(SyncError::validation("Source directory does not exist"));
        }

        let active_dest = if let Some(ref pre_resolved) = self.resolved_dest {
            if pre_resolved.exists() && pre_resolved.is_dir() {
                pre_resolved.clone()
            } else {
                dest_dir.to_path_buf()
            }
        } else {
            dest_dir.to_path_buf()
        };

        if !active_dest.exists() || !active_dest.is_dir() {
            tracing::warn!(
                target = %active_dest.display(),
                "Target destination directory does not exist or is unreachable. Skipping full scan."
            );
            return Ok(ScanOutcome::DestinationUnreachable);
        }

        let mut source_files: HashSet<PathBuf> = HashSet::new();
        let mut scan_complete = true;
        self.scanner.scan_dir_cancellable(
            resolved_source,
            &mut source_files,
            &mut scan_complete,
            cancel,
        )?;

        let cached_records = self.db.list_all_records()?;
        let cached_lookup: HashMap<PathBuf, &FileRecord> = cached_records
            .values()
            .map(|rec| (crate::path_util::normalize_path(&rec.relative_path), rec))
            .collect();

        // Sync all source files
        let mut synced_count = 0usize;
        let mut failed_count = 0usize;
        let mut sync_skip_count = 0usize;
        let mut scratch = vec![0u8; self.config.block_size_bytes() as usize];
        let mut batch: Vec<(FileRecord, Vec<crate::db::BlockHash>)> = Vec::with_capacity(500);
        for rel_path in &source_files {
            if cancel.load(Ordering::Relaxed) {
                self.flush_record_batch(&mut batch)?;
                return Err(SyncError::Cancelled);
            }
            match self.sync_file_to_dest_core(
                rel_path,
                &active_dest,
                &mut scratch,
                cached_lookup.get(rel_path).copied(),
            ) {
                Ok(Some((record, hashes))) => {
                    synced_count += 1;
                    batch.push((record, hashes));
                    if batch.len() >= 500 {
                        self.flush_record_batch(&mut batch)?;
                    }
                }
                Ok(None) => {
                    synced_count += 1;
                }
                Err(e) => {
                    failed_count += 1;
                    let os_code = match &e {
                        SyncError::Io(io_err) => io_err.raw_os_error(),
                        _ => None,
                    };
                    if e.is_network_offline() {
                        tracing::warn!(
                            path = %rel_path.display(),
                            target = %active_dest.display(),
                            error = %e,
                            os_error = ?os_code,
                            remaining = source_files.len() - sync_skip_count - 1,
                            "Target unreachable during full scan, skipping remaining files"
                        );
                        sync_skip_count = source_files.len();
                        break;
                    }
                    tracing::warn!(
                        path = %rel_path.display(),
                        target = %active_dest.display(),
                        error = %e,
                        os_error = ?os_code,
                        "Skipped file during full scan"
                    );
                    sync_skip_count += 1;
                }
            }
        }
        self.flush_record_batch(&mut batch)?;
        if sync_skip_count > 0 {
            tracing::warn!(
                skipped = sync_skip_count,
                total = source_files.len(),
                target = %active_dest.display(),
                "Full scan completed with sync errors"
            );
        }

        // If 100% of files failed to sync (and there were files to sync), destination is inaccessible
        if !source_files.is_empty() && sync_skip_count == source_files.len() {
            return Ok(ScanOutcome::DestinationUnreachable);
        }

        // Detect deletions: files in DB but missing from source
        let mut delete_skip_count = 0usize;
        if self.config.propagate_deletions() {
            if !scan_complete {
                tracing::warn!(
                    "Full scan was incomplete due to inaccessible directories or errors; skipping deletion propagation to prevent data loss"
                );
            } else if source_files.is_empty() && !cached_records.is_empty() {
                tracing::warn!(
                    tracked_count = cached_records.len(),
                    "Source directory is empty but cache contains tracked files. Skipping deletion propagation to prevent accidental target wipe."
                );
                return Ok(ScanOutcome::Success { synced: 0 });
            } else {
                #[cfg(windows)]
                let source_lookup: HashSet<String> = source_files
                    .iter()
                    .map(|p| p.to_string_lossy().replace('\\', "/").to_lowercase())
                    .collect();

                let mut missing_to_delete: Vec<&Path> = Vec::new();
                for tracked_path in cached_records.keys() {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(SyncError::Cancelled);
                    }
                    #[cfg(windows)]
                    let is_present = source_lookup.contains(
                        &tracked_path
                            .to_string_lossy()
                            .replace('\\', "/")
                            .to_lowercase(),
                    );
                    #[cfg(not(windows))]
                    let is_present = source_files.contains(tracked_path);

                    if !is_present {
                        if let Err(e) = self.archive_dest_file_only(tracked_path, &active_dest) {
                            let os_code = match &e {
                                SyncError::Io(io_err) => io_err.raw_os_error(),
                                _ => None,
                            };
                            if e.is_network_offline() {
                                tracing::warn!(
                                    path = %tracked_path.display(),
                                    target = %active_dest.display(),
                                    error = %e,
                                    os_error = ?os_code,
                                    "Target unreachable during deletion phase of full scan, aborting deletion pass"
                                );
                                delete_skip_count += 1;
                                break;
                            }
                            tracing::warn!(
                                path = %tracked_path.display(),
                                target = %active_dest.display(),
                                error = %e,
                                os_error = ?os_code,
                                "Skipped deletion during full scan"
                            );
                            delete_skip_count += 1;
                        } else {
                            missing_to_delete.push(tracked_path.as_path());
                        }
                    }
                }
                if !missing_to_delete.is_empty() {
                    self.db.delete_files_batch(&missing_to_delete)?;
                }
                if delete_skip_count > 0 {
                    tracing::warn!(
                        skipped = delete_skip_count,
                        target = %active_dest.display(),
                        "Full scan completed with deletion errors"
                    );
                }
            }
        }

        if let Err(e) = self.prune_destination_archive(&active_dest) {
            tracing::warn!(
                target = %active_dest.display(),
                error = %e,
                "Failed to prune destination archive after full scan"
            );
        }

        if failed_count > 0 || delete_skip_count > 0 {
            Ok(ScanOutcome::PartialFailure {
                synced: synced_count,
                failed: failed_count,
                delete_failed: delete_skip_count,
            })
        } else {
            Ok(ScanOutcome::Success {
                synced: synced_count,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use crate::db::MockHashStore;
    use crate::sync::SyncEngine;
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
    fn test_full_scan_continues_past_file_errors() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        fs::write(source.join("good.txt"), b"good content").unwrap();

        fs::create_dir_all(source.join("bad")).unwrap();
        fs::write(source.join("bad").join("nested.txt"), b"bad content").unwrap();

        fs::write(dest.join("bad"), b"blocking file").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        assert!(matches!(
            engine.run_full_scan().unwrap(),
            ScanOutcome::PartialFailure {
                synced: 1,
                failed: 1,
                delete_failed: 0,
            }
        ));

        assert!(dest.join("good.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("good.txt")).unwrap(),
            "good content"
        );
        assert!(!dest.join("bad").join("nested.txt").exists());
    }

    #[test]
    fn test_full_scan_all_skipped_returns_false() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        fs::create_dir_all(source.join("bad")).unwrap();
        fs::write(source.join("bad").join("nested.txt"), b"bad content").unwrap();

        fs::write(dest.join("bad"), b"blocking file").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        assert_eq!(
            engine.run_full_scan().unwrap(),
            ScanOutcome::DestinationUnreachable
        );
    }

    #[test]
    fn test_full_scan_dest_missing_skips_early() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("nonexistent_dest_dir");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file1.txt"), b"content").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        assert_eq!(
            engine.run_full_scan().unwrap(),
            ScanOutcome::DestinationUnreachable
        );
    }

    #[test]
    fn test_run_cancellable_full_scan_interruption() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        for i in 0..20 {
            fs::write(
                src.join(format!("file_{}.txt", i)),
                format!("content {}", i),
            )
            .unwrap();
        }

        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let cancel_token = AtomicBool::new(true);
        let result = engine.run_cancellable_full_scan(&dst, &cancel_token);

        assert!(
            result.is_err(),
            "Full scan should abort when cancel token is set"
        );
        match result.unwrap_err() {
            SyncError::Cancelled => {}
            other => panic!("Expected SyncError::Cancelled, got: {:?}", other),
        }
    }

    #[test]
    fn test_empty_source_safety_threshold() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(10)
            .block_size_bytes(4)
            .build()
            .unwrap();
        let store = crate::db::SqliteHashStore::new(
            &db_path,
            crate::db::StoreConfig::new(
                config.block_size_bytes(),
                config.block_sync_threshold_bytes(),
            )
            .unwrap(),
        )
        .unwrap();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);

        fs::write(source.join("important.txt"), b"save me").unwrap();
        engine.run_full_scan().unwrap();
        assert!(dest.join("important.txt").exists());

        fs::remove_file(source.join("important.txt")).unwrap();
        engine.run_full_scan().unwrap();

        assert!(dest.join("important.txt").exists());
    }

    #[test]
    fn test_run_full_scan_uses_save_files_batch() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        for i in 0..5 {
            std::fs::write(src.join(format!("file_{i}.txt")), format!("content {i}")).unwrap();
        }

        let store = MockHashStore::new();
        let config = Config::builder(src).dest_dir(dst).build().unwrap();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        let outcome = engine.run_full_scan().unwrap();
        assert!(matches!(outcome, ScanOutcome::Success { synced: 5 }));

        assert_eq!(
            store.save_file_count(),
            0,
            "Full scan must not call save_file individually"
        );
        assert!(
            store.batch_save_count() >= 1,
            "Full scan must call save_files_batch"
        );
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
    fn test_run_full_scan_case_insensitive_deletions() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        std::fs::write(src.join("readme.txt"), b"hello").unwrap();
        std::fs::write(dst.join("README.TXT"), b"hello").unwrap();

        let db = MockHashStore::new();
        let old_record = FileRecord {
            id: Some(1),
            relative_path: PathBuf::from("README.TXT"),
            file_size: 5,
            last_modified: 1000,
        };
        db.save_file(&old_record, &[]).unwrap();

        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(db, target_cfg);

        let outcome = engine.run_full_scan().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });
        assert!(!dst.join(".syncdir_archive").exists());
    }

    #[test]
    fn test_full_scan_path_separator_normalization() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let nested_dir = src.join("nested");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let test_file = nested_dir.join("file.txt");
        fs::write(&test_file, b"content").unwrap();

        let config = Config::test_default(src.clone(), dst.clone());
        let store = MockHashStore::new();
        let rec = FileRecord::new(
            PathBuf::from("nested/file.txt"),
            7,
            super::super::engine::safe_modified_millis(&fs::metadata(&test_file).unwrap()).unwrap(),
        )
        .with_id(1);
        store.save_file(&rec, &[]).unwrap();

        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);
        let outcome = engine.run_full_scan().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });
        assert!(dst.join("nested").join("file.txt").exists());
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
    fn test_full_scan_db_error_propagated() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src, dst);
        let store = MockHashStore::new();
        store.set_error_hook(Some(Box::new(|op| {
            if op == "list_all_records" {
                Some(SyncError::db("Forced list_all_records failure"))
            } else {
                None
            }
        })));
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);
        let result = engine.run_full_scan();
        assert!(matches!(result, Err(SyncError::Db(..))));
    }

    #[test]
    fn test_run_full_scan_uses_pre_resolved_dest() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let unreachable_dst = temp.path().join("unreachable_dst");
        let alt_dst = temp.path().join("alt_dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&alt_dst).unwrap();
        fs::write(src.join("hello.txt"), b"test pre-resolved dest").unwrap();

        let config = Config::test_default(src, unreachable_dst);
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine =
            LocalSyncEngine::new(MockHashStore::new(), target_cfg).with_resolved_dest(&alt_dst);
        assert_eq!(engine.resolved_dest(), Some(alt_dst.as_path()));

        let outcome = engine.run_full_scan().unwrap();
        assert!(matches!(outcome, ScanOutcome::Success { synced: 1 }));
        assert!(alt_dst.join("hello.txt").exists());
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
}

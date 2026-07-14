//! Sync command types and engine trait for syncdir.
//!
//! Defines the message types that the monitor and tray threads
//! send to the sync worker, and the trait contract for the sync engine.

use crate::config::Config;
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Commands sent from the file watcher or tray UI to the sync worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// A file was created or modified at the given relative path.
    FileModified(PathBuf),
    /// A file was deleted at the given relative path.
    FileDeleted(PathBuf),
    /// Request a full directory scan and sync.
    TriggerFullScan,
}

/// Core sync execution contract. Implemented by the delta sync engine.
pub trait SyncEngine {
    /// Synchronize a single file from source to destination.
    fn sync_file(&self, path: &str) -> Result<(), SyncError>;
    /// Handle deletion of a file (archive on destination).
    fn delete_file(&self, path: &str) -> Result<(), SyncError>;
    /// Perform a full directory scan and sync all changed files.
    fn run_full_scan(&self) -> Result<(), SyncError>;
}

/// Delta sync engine backed by a `HashStore` for signature caching.
pub struct LocalSyncEngine<S: HashStore> {
    pub(crate) db: S,
    pub(crate) config: Config,
}

impl<S: HashStore> LocalSyncEngine<S> {
    /// Create a new sync engine with the given database and config.
    pub fn new(db: S, config: Config) -> Self {
        Self { db, config }
    }

    /// Hash a file in block-sized chunks, returning (size, mtime, hashes).
    fn calculate_hashes(&self, file_path: &Path) -> Result<(i64, i64, Vec<Vec<u8>>), SyncError> {
        let mut file = File::open(file_path)?;
        let metadata = file.metadata()?;
        let file_size = metadata.len() as i64;
        let last_modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SyncError::Validation(e.to_string()))?
            .as_millis() as i64;

        let block_size = self.config.block_size_bytes as usize;
        let mut buffer = vec![0; block_size];
        let mut hashes = Vec::new();

        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            let hash = blake3::hash(&buffer[..bytes_read]);
            hashes.push(hash.as_bytes().to_vec());
        }

        Ok((file_size, last_modified, hashes))
    }

    /// Build the archive path: `<dest>/.syncdir_archive/<ts>_<relative_path>`.
    fn get_archive_path(&self, relative_path: &Path, timestamp: &str) -> PathBuf {
        let mut components = relative_path.components();
        if let Some(first) = components.next() {
            let first_str = first.as_os_str().to_string_lossy();
            let prefixed = format!("{}_{}", timestamp, first_str);
            let mut archive_rel = PathBuf::from(prefixed);
            for rest in components {
                archive_rel.push(rest);
            }
            self.config
                .dest_dir
                .join(".syncdir_archive")
                .join(archive_rel)
        } else {
            self.config.dest_dir.join(".syncdir_archive")
        }
    }
}

impl<S: HashStore> SyncEngine for LocalSyncEngine<S> {
    fn sync_file(&self, path: &str) -> Result<(), SyncError> {
        let rel_path = PathBuf::from(path);
        if !is_safe_relative_path(&rel_path) {
            return Err(SyncError::Validation(format!(
                "Unsafe path traversal detected: {}",
                path
            )));
        }
        let src_path = self.config.source_dir.join(&rel_path);
        let dest_path = self.config.dest_dir.join(&rel_path);

        if !src_path.exists() {
            return Err(SyncError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Source file not found",
            )));
        }

        let (src_size, src_mod, src_hashes) = self.calculate_hashes(&src_path)?;

        // Fast-path: metadata match means already in sync
        if dest_path.exists() {
            let dest_meta = fs::metadata(&dest_path)?;
            let dest_size = dest_meta.len() as i64;
            let dest_mod = dest_meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_err(|e| SyncError::Validation(e.to_string()))?
                .as_millis() as i64;

            if let Some(record) = self.db.get_file(path)?
                && record.file_size == src_size
                && record.last_modified == src_mod
                && dest_size == src_size
                && dest_mod == src_mod
            {
                return Ok(());
            }
        }

        // Small file: full copy
        if (src_size as u64) < self.config.block_sync_threshold_bytes {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dest_path)?;

            // Windows requires write access for set_times
            let dest_file = OpenOptions::new().write(true).open(&dest_path)?;
            dest_file.set_times(fs::FileTimes::new().set_modified(
                SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(src_mod as u64),
            ))?;

            let record = FileRecord {
                id: None,
                relative_path: path.to_string(),
                file_size: src_size,
                last_modified: src_mod,
            };
            self.db.save_file(&record, &src_hashes)?;
            return Ok(());
        }

        // Large file: in-place delta sync
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut dest_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&dest_path)?;

        let file_record = self.db.get_file(path)?;
        let old_hashes = match &file_record {
            Some(rec) => {
                let id = rec.id.ok_or_else(|| {
                    SyncError::Validation("Corrupted file record: missing ID".to_string())
                })?;
                self.db.get_block_hashes(id)?
            }
            None => Vec::new(),
        };

        let mut src_file = File::open(&src_path)?;
        let block_size = self.config.block_size_bytes;
        let mut buffer = vec![0; block_size as usize];

        for (idx, hash) in src_hashes.iter().enumerate() {
            if old_hashes.get(idx) != Some(hash) {
                src_file.seek(SeekFrom::Start(idx as u64 * block_size))?;
                let bytes_read = src_file.read(&mut buffer)?;
                if bytes_read > 0 {
                    dest_file.seek(SeekFrom::Start(idx as u64 * block_size))?;
                    dest_file.write_all(&buffer[..bytes_read])?;

                    // Write-verification: read back and check hash
                    if self.config.verify_writes {
                        dest_file.seek(SeekFrom::Start(idx as u64 * block_size))?;
                        let mut verify_buf = vec![0; bytes_read];
                        dest_file.read_exact(&mut verify_buf)?;
                        let verify_hash = blake3::hash(&verify_buf);
                        if verify_hash.as_bytes() != hash.as_slice() {
                            return Err(SyncError::Validation(
                                "Write verification failed".to_string(),
                            ));
                        }
                    }
                }
            }
        }

        // Truncate if file shrank
        dest_file.set_len(src_size as u64)?;
        dest_file.set_times(fs::FileTimes::new().set_modified(
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(src_mod as u64),
        ))?;

        let record = FileRecord {
            id: file_record.and_then(|r| r.id),
            relative_path: path.to_string(),
            file_size: src_size,
            last_modified: src_mod,
        };
        self.db.save_file(&record, &src_hashes)?;
        Ok(())
    }

    fn delete_file(&self, path: &str) -> Result<(), SyncError> {
        let rel_path = PathBuf::from(path);
        if !is_safe_relative_path(&rel_path) {
            return Err(SyncError::Validation(format!(
                "Unsafe path traversal detected: {}",
                path
            )));
        }
        let dest_path = self.config.dest_dir.join(&rel_path);

        if dest_path.exists() && self.config.propagate_deletions {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| SyncError::Validation(e.to_string()))?
                .as_secs()
                .to_string();
            let archive_path = self.get_archive_path(&rel_path, &timestamp);
            if let Some(parent) = archive_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&dest_path, &archive_path)?;
        }
        self.db.delete_file(path)?;
        Ok(())
    }

    fn run_full_scan(&self) -> Result<(), SyncError> {
        if !self.config.source_dir.exists() {
            return Err(SyncError::Validation(
                "Source directory does not exist".to_string(),
            ));
        }

        let mut source_files = HashSet::new();

        fn scan_dir(
            dir: &Path,
            source_root: &Path,
            files: &mut HashSet<String>,
        ) -> Result<(), std::io::Error> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    scan_dir(&path, source_root, files)?;
                } else if path.is_file()
                    && let Ok(rel) = path.strip_prefix(source_root)
                {
                    files.insert(rel.to_string_lossy().to_string());
                }
            }
            Ok(())
        }

        scan_dir(
            &self.config.source_dir,
            &self.config.source_dir,
            &mut source_files,
        )?;

        // Sync all source files
        for rel_path in &source_files {
            self.sync_file(rel_path)?;
        }

        // Detect deletions: files in DB but missing from source
        if self.config.propagate_deletions {
            // Empty source directory safety check:
            // If we found zero files on disk, but the local database cache contains
            // tracked files, skip deletion propagation. This protects against
            // mount-points or drives that appear empty due to unmounting.
            if source_files.is_empty() {
                let tracked = self.db.list_files()?;
                if !tracked.is_empty() {
                    tracing::warn!(
                        tracked_count = tracked.len(),
                        "Source directory is empty but cache contains tracked files. Skipping deletion propagation to prevent accidental target wipe."
                    );
                    return Ok(());
                }
            }

            let tracked = self.db.list_files()?;
            for tracked_path in tracked {
                if !source_files.contains(&tracked_path) {
                    self.delete_file(&tracked_path)?;
                }
            }
        }

        Ok(())
    }
}

fn is_safe_relative_path(path: &Path) -> bool {
    if !path.is_relative() {
        return false;
    }
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => return false,
            std::path::Component::RootDir => return false,
            std::path::Component::Prefix(_) => return false,
            _ => {}
        }
    }
    true
}

/// Spawns a background synchronization worker thread.
///
/// The worker listens for filesystem events (file changes/deletions) on its channel
/// and triggers block-level sync operations to its specific destination directory.
///
/// # Arguments
///
/// * `target_index` - The unique index of the destination target.
/// * `config` - Synchronization daemon configuration copy.
/// * `db` - Persistent SQLite signature store.
/// * `rx` - Channel receiver for processing `SyncCommand`s.
/// * `event_proxy` - Winit proxy to signal directory status updates to the system tray.
/// * `source_online_atomic` - Shared thread-safe flag indicating source presence.
///
/// # Returns
///
/// Returns the join handle for the spawned background worker thread.
pub fn start_sync_worker<S: HashStore + Send + 'static>(
    target_index: usize,
    config: Config,
    db: S,
    rx: std::sync::mpsc::Receiver<SyncCommand>,
    event_proxy: Option<winit::event_loop::EventLoopProxy<crate::tray::UserEvent>>,
    source_online_atomic: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let engine = LocalSyncEngine::new(db, config.clone());
        let mut pending_syncs: HashMap<PathBuf, Instant> = HashMap::new();
        let mut pending_deletes: HashMap<PathBuf, Instant> = HashMap::new();

        let mut source_online = false;
        let mut dest_online = false;
        let mut last_sent_dest_online = None;

        let mut last_status_check = Instant::now()
            .checked_sub(std::time::Duration::from_secs(
                config.retry_interval_seconds,
            ))
            .unwrap_or_else(Instant::now);

        loop {
            let now = Instant::now();

            if now.duration_since(last_status_check)
                >= std::time::Duration::from_secs(config.retry_interval_seconds)
            {
                last_status_check = now;

                let current_source_online =
                    source_online_atomic.load(std::sync::atomic::Ordering::Relaxed);
                let current_dest_online = config.dest_dir.exists() && config.dest_dir.is_dir();

                source_online = current_source_online;
                dest_online = current_dest_online;

                if last_sent_dest_online != Some(current_dest_online) {
                    last_sent_dest_online = Some(current_dest_online);

                    if let Some(ref proxy) = event_proxy {
                        let _ = proxy.send_event(crate::tray::UserEvent::StatusUpdate(
                            crate::tray::TargetStatusUpdate {
                                target_index,
                                dest_online,
                            },
                        ));
                    }
                }
            }

            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(SyncCommand::FileModified(path)) => {
                    let deadline =
                        Instant::now() + std::time::Duration::from_secs(config.debounce_seconds);
                    pending_syncs.insert(path.clone(), deadline);
                    pending_deletes.remove(&path);
                }
                Ok(SyncCommand::FileDeleted(path)) => {
                    let deadline =
                        Instant::now() + std::time::Duration::from_secs(config.debounce_seconds);
                    pending_deletes.insert(path.clone(), deadline);
                    pending_syncs.remove(&path);
                }
                Ok(SyncCommand::TriggerFullScan) => {
                    if source_online {
                        if let Err(e) = engine.run_full_scan() {
                            tracing::error!(error = %e, "Full scan failed");
                        }
                    } else {
                        tracing::warn!("Skipping full scan: source directory is offline");
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            let now = Instant::now();

            let ready_syncs: Vec<_> = pending_syncs
                .iter()
                .filter(|(_, deadline)| now >= **deadline)
                .map(|(path, _)| path.clone())
                .collect();
            for path in ready_syncs {
                pending_syncs.remove(&path);
                if source_online && dest_online {
                    if let Err(e) = engine.sync_file(&path.to_string_lossy()) {
                        tracing::warn!(path = %path.display(), error = %e, "Sync failed");
                    }
                } else {
                    tracing::warn!(path = %path.display(), "Skipped syncing file: source or destination offline");
                    let deadline =
                        Instant::now() + std::time::Duration::from_secs(config.debounce_seconds);
                    pending_syncs.insert(path, deadline);
                }
            }

            let ready_deletes: Vec<_> = pending_deletes
                .iter()
                .filter(|(_, deadline)| now >= **deadline)
                .map(|(path, _)| path.clone())
                .collect();
            for path in ready_deletes {
                pending_deletes.remove(&path);
                if dest_online {
                    if let Err(e) = engine.delete_file(&path.to_string_lossy()) {
                        tracing::warn!(path = %path.display(), error = %e, "Delete failed");
                    }
                } else {
                    tracing::warn!(path = %path.display(), "Skipped deleting file: destination offline");
                    let deadline =
                        Instant::now() + std::time::Duration::from_secs(config.debounce_seconds);
                    pending_deletes.insert(path, deadline);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SqliteHashStore;
    use tempfile::tempdir;

    fn test_config(source: PathBuf, dest: PathBuf) -> Config {
        Config {
            source_dir: source,
            dest_dir: dest,
            debounce_seconds: 1,
            propagate_deletions: true,
            block_sync_threshold_bytes: 10,
            block_size_bytes: 4,
            verify_writes: true,
            retry_interval_seconds: 10,
            dest_dirs: None,
        }
    }

    #[test]
    fn test_small_file_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        // Write a small file (< 10 bytes threshold)
        fs::write(source.join("tiny.txt"), b"hi").unwrap();
        engine.sync_file("tiny.txt").unwrap();

        let content = fs::read_to_string(dest.join("tiny.txt")).unwrap();
        assert_eq!(content, "hi");

        // DB should have a record
        assert!(engine.db.get_file("tiny.txt").unwrap().is_some());
    }

    #[test]
    fn test_delta_sync_large_file() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        // 12 bytes > 10 byte threshold → delta sync path (3 blocks of 4)
        fs::write(source.join("big.bin"), b"AAAABBBBcccc").unwrap();
        engine.sync_file("big.bin").unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAABBBBcccc");

        // Modify only block 1 (bytes 4-7)
        let big_bin_path = source.join("big.bin");
        fs::write(&big_bin_path, b"AAAAZZZZCCCC").unwrap();
        // Advance modified time to avoid fast-path matching in the same second
        let f = OpenOptions::new().write(true).open(&big_bin_path).unwrap();
        f.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
        )
        .unwrap();

        engine.sync_file("big.bin").unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAAZZZZCCCC");

        // DB should have updated hashes
        let record = engine.db.get_file("big.bin").unwrap().unwrap();
        let hashes = engine.db.get_block_hashes(record.id.unwrap()).unwrap();
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn test_deletion_archive() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        // Sync a file first
        fs::write(source.join("doomed.txt"), b"bye").unwrap();
        engine.sync_file("doomed.txt").unwrap();
        assert!(dest.join("doomed.txt").exists());

        // Delete it
        engine.delete_file("doomed.txt").unwrap();

        // Original dest file should be gone
        assert!(!dest.join("doomed.txt").exists());

        // Should be in .syncdir_archive
        let archive = dest.join(".syncdir_archive");
        assert!(archive.exists());
        let entries: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let archived_name = entries[0].file_name().to_string_lossy().to_string();
        assert!(archived_name.ends_with("_doomed.txt"));

        // DB record should be gone
        assert!(engine.db.get_file("doomed.txt").unwrap().is_none());
    }

    #[test]
    fn test_empty_source_safety_threshold() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        // Sync a file first so the database has a record
        fs::write(source.join("important.txt"), b"save me").unwrap();
        engine.run_full_scan().unwrap();
        assert!(dest.join("important.txt").exists());

        // Now delete the source file so the source directory is completely empty
        fs::remove_file(source.join("important.txt")).unwrap();

        // Run full scan again. Since propagate_deletions is true and source is empty,
        // it should hit the safety threshold check, log a warning, and skip deletion propagation.
        engine.run_full_scan().unwrap();

        // The dest file should still exist and not be deleted/archived!
        assert!(dest.join("important.txt").exists());
    }
}

//! Sync command types and engine trait for syncdir.
//!
//! Defines the message types that the monitor and tray threads
//! send to the sync worker, and the trait contract for the sync engine.

use crate::config::Config;
use crate::db::{BlockHash, FileRecord, HashStore};
use crate::error::SyncError;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Extract file modified time as milliseconds since UNIX epoch.
///
/// Pre-1970 timestamps are clamped to 0 (epoch) with a warning log.
///
/// # Errors
///
/// Returns `SyncError::Io` if the file's modified time cannot be read.
fn safe_modified_millis(metadata: &std::fs::Metadata) -> Result<i64, SyncError> {
    let modified = metadata.modified().map_err(SyncError::Io)?;
    match modified.duration_since(UNIX_EPOCH) {
        Ok(dur) => Ok(dur.as_millis() as i64),
        Err(_) => {
            tracing::warn!("File has pre-1970 modified timestamp, clamping to epoch");
            Ok(0)
        }
    }
}

/// Convert a millisecond timestamp to a `Duration`, clamping negative values to zero.
fn safe_epoch_duration_millis(millis: i64) -> std::time::Duration {
    std::time::Duration::from_millis(millis.max(0) as u64)
}

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

/// Outcome of a full directory scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    /// All files synced successfully.
    Success { synced: usize },
    /// Some files failed to sync.
    PartialFailure { synced: usize, failed: usize },
    /// Destination is unreachable.
    DestinationUnreachable,
}

/// Observer for sync worker status changes. Decouples sync from UI.
pub trait SyncStatusObserver: Send + Sync + 'static {
    fn on_target_status_change(&self, target_index: usize, online: bool);
}

/// Read exactly `buf.len()` bytes or until EOF, handling partial reads.
fn read_block<R: std::io::Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// Core sync execution contract. Implemented by the delta sync engine.
pub trait SyncEngine {
    /// Synchronize a single file from source to destination.
    fn sync_file(&self, path: &str) -> Result<(), SyncError>;
    /// Handle deletion of a file (archive on destination).
    fn delete_file(&self, path: &str) -> Result<(), SyncError>;
    /// Perform a full directory scan and sync all changed files.
    fn run_full_scan(&self) -> Result<ScanOutcome, SyncError>;
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
    fn calculate_hashes(&self, file_path: &Path) -> Result<(i64, i64, Vec<BlockHash>), SyncError> {
        let mut file = File::open(file_path)?;
        let metadata = file.metadata()?;
        let file_size = metadata.len() as i64;
        let last_modified = safe_modified_millis(&metadata)?;

        let block_size = self.config.block_size_bytes as usize;
        let mut buffer = vec![0; block_size];
        let mut hashes = Vec::new();

        loop {
            let bytes_read = read_block(&mut file, &mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            let hash = blake3::hash(&buffer[..bytes_read]);
            hashes.push(*hash.as_bytes());
        }

        Ok((file_size, last_modified, hashes))
    }

    /// Build the archive path: `<dest>/.syncdir_archive/<ts>_<relative_path>`.
    fn get_archive_path(
        &self,
        relative_path: &Path,
        timestamp: &str,
    ) -> Result<PathBuf, SyncError> {
        let dest_dir = self.config.dest_dir.as_ref().ok_or_else(|| {
            SyncError::Validation(
                "Destination directory not configured for archive creation".to_string(),
            )
        })?;

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
        let src_path = self.config.resolved_source_dir().join(&rel_path);
        let dest_dir = self.config.dest_dir.as_ref().ok_or_else(|| {
            SyncError::Validation("Destination directory not configured".to_string())
        })?;
        let dest_path = dest_dir.join(&rel_path);

        let src_meta = fs::metadata(&src_path).map_err(SyncError::Io)?;
        let src_size = src_meta.len() as i64;
        let src_mod = safe_modified_millis(&src_meta)?;

        // Fast-path: metadata match means already in sync (avoids hashing)
        if dest_path.exists()
            && let Ok(dest_meta) = fs::metadata(&dest_path)
        {
            let dest_size = dest_meta.len() as i64;
            let dest_mod = safe_modified_millis(&dest_meta)?;

            if let Some(ref record) = self.db.get_file(path)?
                && record.file_size == src_size
                && record.last_modified == src_mod
                && dest_size == src_size
                && (dest_mod - src_mod).abs() <= 2000
            {
                tracing::debug!(path = path, "Metadata unchanged, skipping sync");
                return Ok(());
            }
        }

        let (_, _, src_hashes) = self.calculate_hashes(&src_path)?;

        // Small file: full copy
        if (src_size as u64) < self.config.block_sync_threshold_bytes {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dest_path)?;

            // Windows requires write access for set_times
            let dest_file = OpenOptions::new().write(true).open(&dest_path)?;
            dest_file.set_times(
                fs::FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + safe_epoch_duration_millis(src_mod)),
            )?;

            let record = FileRecord {
                id: None,
                relative_path: path.to_string(),
                file_size: src_size,
                last_modified: src_mod,
            };
            self.db.save_file(&record, &src_hashes)?;
            tracing::info!(
                path = %rel_path.display(),
                target = %dest_dir.display(),
                size = src_size,
                "Synced file to destination"
            );
            return Ok(());
        }

        // Large file: in-place delta sync
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let dest_existed = dest_path.exists();
        let mut dest_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&dest_path)?;

        let file_record = self.db.get_file(path)?;
        let old_hashes = if dest_existed {
            match &file_record {
                Some(rec) => {
                    let id = rec.id.ok_or_else(|| {
                        SyncError::Validation("Corrupted file record: missing ID".to_string())
                    })?;
                    self.db.get_block_hashes(id)?
                }
                None => Vec::new(),
            }
        } else {
            Vec::new() // Force all blocks written if destination was deleted
        };

        let mut src_file = File::open(&src_path)?;
        let block_size = self.config.block_size_bytes;
        let mut buffer = vec![0; block_size as usize];
        let mut verify_buf = if self.config.verify_writes {
            vec![0; block_size as usize]
        } else {
            Vec::new()
        };

        for (idx, hash) in src_hashes.iter().enumerate() {
            if old_hashes.get(idx) != Some(hash) {
                src_file.seek(SeekFrom::Start(idx as u64 * block_size))?;
                let bytes_read = read_block(&mut src_file, &mut buffer)?;
                if bytes_read > 0 {
                    dest_file.seek(SeekFrom::Start(idx as u64 * block_size))?;
                    dest_file.write_all(&buffer[..bytes_read])?;

                    // Write-verification: read back and check hash
                    if self.config.verify_writes {
                        dest_file.seek(SeekFrom::Start(idx as u64 * block_size))?;
                        let v_buf = &mut verify_buf[..bytes_read];
                        dest_file.read_exact(v_buf)?;
                        let verify_hash = blake3::hash(v_buf);
                        if verify_hash.as_bytes() != hash {
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
        dest_file.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + safe_epoch_duration_millis(src_mod)),
        )?;

        let record = FileRecord {
            id: file_record.and_then(|r| r.id),
            relative_path: path.to_string(),
            file_size: src_size,
            last_modified: src_mod,
        };
        self.db.save_file(&record, &src_hashes)?;
        tracing::info!(
            path = %rel_path.display(),
            target = %dest_dir.display(),
            size = src_size,
            "Synced file to destination (delta)"
        );
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
        let dest_dir = self.config.dest_dir.as_ref().ok_or_else(|| {
            SyncError::Validation("Destination directory not configured".to_string())
        })?;
        let dest_path = dest_dir.join(&rel_path);

        if !dest_dir.exists() {
            return Err(SyncError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Destination unreachable, skipping DB deletion for {}", path),
            )));
        }

        if dest_path.exists() && self.config.propagate_deletions {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| {
                    SyncError::Io(std::io::Error::other(format!("System clock error: {e}")))
                })?
                .as_millis()
                .to_string();

            let mut archive_path = self.get_archive_path(&rel_path, &timestamp)?;
            let mut counter = 1u32;
            while archive_path.exists() {
                archive_path =
                    self.get_archive_path(&rel_path, &format!("{}_{}", timestamp, counter))?;
                counter += 1;
            }

            if let Some(parent) = archive_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&dest_path, &archive_path)?;
        }
        self.db.delete_file(path)?;
        Ok(())
    }

    fn run_full_scan(&self) -> Result<ScanOutcome, SyncError> {
        let resolved_source = self.config.resolved_source_dir();
        if !resolved_source.exists() {
            return Err(SyncError::Validation(
                "Source directory does not exist".to_string(),
            ));
        }

        if let Some(ref dest) = self.config.dest_dir {
            let dest_reachable = dest.exists() && dest.is_dir();
            let is_reachable = if !dest_reachable {
                let alt_path = crate::config::try_resolve_alternate_path(dest);
                if alt_path.exists() && alt_path.is_dir() {
                    if alt_path != *dest {
                        tracing::info!(
                            target = %dest.display(),
                            resolved_path = %alt_path.display(),
                            "Target destination resolved alternate mapped drive/UNC SMB path for full scan."
                        );
                    }
                    true
                } else {
                    false
                }
            } else {
                true
            };

            if !is_reachable {
                tracing::warn!(
                    target = %dest.display(),
                    "Target destination directory does not exist or is unreachable. Skipping full scan."
                );
                return Ok(ScanOutcome::DestinationUnreachable);
            }
        }

        let mut source_files = HashSet::new();
        scan_dir(&resolved_source, &resolved_source, &mut source_files, 0)?;

        // Sync all source files
        let mut synced_count = 0usize;
        let mut failed_count = 0usize;
        let mut sync_skip_count = 0usize;
        for rel_path in &source_files {
            match self.sync_file(rel_path) {
                Ok(()) => {
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
                            path = %rel_path,
                            target = %self.config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                            error = %e,
                            os_error = ?os_code,
                            remaining = source_files.len() - sync_skip_count - 1,
                            "Target unreachable during full scan, skipping remaining files"
                        );
                        sync_skip_count = source_files.len();
                        break;
                    }
                    tracing::warn!(
                        path = %rel_path,
                        target = %self.config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                        error = %e,
                        os_error = ?os_code,
                        "Skipped file during full scan"
                    );
                    sync_skip_count += 1;
                }
            }
        }
        if sync_skip_count > 0 {
            tracing::warn!(
                skipped = sync_skip_count,
                total = source_files.len(),
                target = %self.config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                "Full scan completed with sync errors"
            );
        }

        // If 100% of files failed to sync (and there were files to sync), destination is inaccessible
        if !source_files.is_empty() && sync_skip_count == source_files.len() {
            return Ok(ScanOutcome::DestinationUnreachable);
        }

        // Detect deletions: files in DB but missing from source
        if self.config.propagate_deletions {
            // Empty source directory safety check:
            if source_files.is_empty() {
                let tracked = self.db.list_files()?;
                if !tracked.is_empty() {
                    tracing::warn!(
                        tracked_count = tracked.len(),
                        "Source directory is empty but cache contains tracked files. Skipping deletion propagation to prevent accidental target wipe."
                    );
                    return Ok(ScanOutcome::Success { synced: 0 });
                }
            }

            let tracked = self.db.list_files()?;
            let mut delete_skip_count = 0usize;
            for tracked_path in tracked {
                if !source_files.contains(&tracked_path)
                    && let Err(e) = self.delete_file(&tracked_path)
                {
                    let os_code = match &e {
                        SyncError::Io(io_err) => io_err.raw_os_error(),
                        _ => None,
                    };
                    tracing::warn!(
                        path = %tracked_path,
                        target = %self.config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                        error = %e,
                        os_error = ?os_code,
                        "Skipped deletion during full scan"
                    );
                    delete_skip_count += 1;
                }
            }
            if delete_skip_count > 0 {
                tracing::warn!(
                    skipped = delete_skip_count,
                    target = %self.config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                    "Full scan completed with deletion errors"
                );
            }
        }

        if failed_count > 0 {
            Ok(ScanOutcome::PartialFailure {
                synced: synced_count,
                failed: failed_count,
            })
        } else {
            Ok(ScanOutcome::Success {
                synced: synced_count,
            })
        }
    }
}

fn scan_dir(
    dir: &Path,
    source_root: &Path,
    files: &mut HashSet<String>,
    depth: usize,
) -> Result<(), std::io::Error> {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        tracing::warn!(path = %dir.display(), "Max directory depth exceeded, skipping");
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            tracing::debug!(path = %entry.path().display(), "Skipping symlink in scan");
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            scan_dir(&path, source_root, files, depth + 1)?;
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(source_root)
        {
            files.insert(rel.to_string_lossy().to_string());
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn is_safe_relative_path(path: &Path) -> bool {
    if !path.is_relative() || path.as_os_str().is_empty() {
        return false;
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
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
                let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
                if RESERVED.contains(&stem.as_str()) {
                    return false;
                }
            }
            _ => return false,
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
#[must_use = "dropping the JoinHandle detaches the sync worker thread"]
pub fn start_sync_worker<S: HashStore + Send + 'static>(
    target_index: usize,
    config: Config,
    db: S,
    rx: std::sync::mpsc::Receiver<SyncCommand>,
    observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
    source_online_atomic: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let engine = LocalSyncEngine::new(db, config.clone());
        let mut pending_syncs: HashMap<PathBuf, Instant> = HashMap::new();
        let mut pending_deletes: HashMap<PathBuf, Instant> = HashMap::new();

        let mut source_online = source_online_atomic.load(std::sync::atomic::Ordering::Relaxed);
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
                let current_dest_online = config
                    .dest_dir
                    .as_ref()
                    .map(|d| match std::fs::metadata(d) {
                        Ok(meta) => meta.is_dir(),
                        Err(ref e) => {
                            if dest_online {
                                tracing::warn!(
                                    target_index = target_index + 1,
                                    target_path = %d.display(),
                                    error = %e,
                                    os_error = ?e.raw_os_error(),
                                    "Target destination went offline."
                                );
                            }
                            false
                        }
                    })
                    .unwrap_or(false);

                source_online = current_source_online;
                dest_online = current_dest_online;

                if last_sent_dest_online != Some(current_dest_online) {
                    if current_dest_online && last_sent_dest_online == Some(false) {
                        tracing::info!(
                            target_index = target_index + 1,
                            target_path = %config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                            "Target destination is back online."
                        );
                    }
                    last_sent_dest_online = Some(current_dest_online);

                    if let Some(ref obs) = observer {
                        obs.on_target_status_change(target_index, dest_online);
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
                    if source_online_atomic.load(std::sync::atomic::Ordering::Relaxed) {
                        match engine.run_full_scan() {
                            Ok(ScanOutcome::Success { synced }) => {
                                tracing::info!(synced, "Full scan completed successfully");
                                if !dest_online {
                                    dest_online = true;
                                    last_sent_dest_online = Some(true);
                                    if let Some(ref obs) = observer {
                                        obs.on_target_status_change(target_index, true);
                                    }
                                }
                            }
                            Ok(ScanOutcome::PartialFailure { synced, failed }) => {
                                tracing::warn!(
                                    synced,
                                    failed,
                                    target_path = %config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                                    "Full scan completed with partial sync failures"
                                );
                            }
                            Ok(ScanOutcome::DestinationUnreachable) => {
                                tracing::warn!(
                                    target_path = %config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                                    "Full scan destination unreachable. Setting destination status to offline."
                                );
                                dest_online = false;
                                last_sent_dest_online = Some(false);
                                if let Some(ref obs) = observer {
                                    obs.on_target_status_change(target_index, false);
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Full scan failed");
                            }
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
                        let os_code = match &e {
                            SyncError::Io(io_err) => io_err.raw_os_error(),
                            _ => None,
                        };
                        tracing::warn!(
                            path = %path.display(),
                            target = %config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                            error = %e,
                            os_error = ?os_code,
                            "Sync failed, scheduling retry"
                        );
                        let retry_at = Instant::now()
                            + std::time::Duration::from_secs(config.retry_interval_seconds);
                        pending_syncs.insert(path, retry_at);
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
                        let os_code = match &e {
                            SyncError::Io(io_err) => io_err.raw_os_error(),
                            _ => None,
                        };
                        tracing::warn!(
                            path = %path.display(),
                            target = %config.dest_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
                            error = %e,
                            os_error = ?os_code,
                            "Deletion failed, scheduling retry"
                        );
                        let retry_at = Instant::now()
                            + std::time::Duration::from_secs(config.retry_interval_seconds);
                        pending_deletes.insert(path, retry_at);
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
    use crate::db::{MockHashStore, SqliteHashStore};
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn test_config(source: PathBuf, dest: PathBuf) -> Config {
        Config::test_default(source, dest)
    }

    #[test]
    fn test_read_block_fills_complete_buffer() {
        struct ChunkyReader {
            data: Vec<u8>,
            pos: usize,
            chunk: usize,
        }
        impl std::io::Read for ChunkyReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let rem = &self.data[self.pos..];
                let n = buf.len().min(self.chunk).min(rem.len());
                buf[..n].copy_from_slice(&rem[..n]);
                self.pos += n;
                Ok(n)
            }
        }
        let data = vec![0xAB; 1024];
        let mut reader = ChunkyReader {
            data: data.clone(),
            pos: 0,
            chunk: 64,
        };
        let mut buf = vec![0u8; 1024];
        let n = read_block(&mut reader, &mut buf).unwrap();
        assert_eq!(n, 1024);
        assert_eq!(buf, data);
    }

    #[test]
    fn test_is_safe_relative_path_rejects_empty() {
        assert!(!is_safe_relative_path(std::path::Path::new("")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_reserved_names() {
        assert!(!is_safe_relative_path(std::path::Path::new("CON")));
        assert!(!is_safe_relative_path(std::path::Path::new(
            "subdir\\NUL.txt"
        )));
        assert!(!is_safe_relative_path(std::path::Path::new("COM1")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_ads() {
        assert!(!is_safe_relative_path(std::path::Path::new(
            "file.txt:stream"
        )));
    }

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
        scan_dir(&src, &src, &mut files, 0).unwrap();
        assert!(files.contains("real.txt"));
        assert!(!files.iter().any(|f| f.contains("secret")));
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

    #[test]
    fn test_zero_byte_file_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // Create a 0-byte file
        fs::write(source.join("empty.txt"), b"").unwrap();
        engine.sync_file("empty.txt").unwrap();

        assert!(dest.join("empty.txt").exists());
        assert_eq!(fs::read(dest.join("empty.txt")).unwrap(), b"");
        assert!(engine.db.get_file("empty.txt").unwrap().is_some());
    }

    #[test]
    fn test_exact_block_multiple_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let mut config = test_config(source.clone(), dest.clone());
        config.block_sync_threshold_bytes = 4;
        config.block_size_bytes = 4;
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // 8 bytes payload = exactly 2 blocks of 4 bytes
        fs::write(source.join("exact.bin"), b"12345678").unwrap();
        engine.sync_file("exact.bin").unwrap();

        assert_eq!(fs::read(dest.join("exact.bin")).unwrap(), b"12345678");
        let rec = engine.db.get_file("exact.bin").unwrap().unwrap();
        let hashes = engine.db.get_block_hashes(rec.id.unwrap()).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn test_worker_queue_debouncing_storm() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let mut config = test_config(source.clone(), dest.clone());
        config.debounce_seconds = 1;
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let _handle = start_sync_worker(0, config, store, rx, None, source_online);

        // Write source file
        fs::write(source.join("storm.txt"), b"storm data").unwrap();

        // Send rapid burst of interleaved modified/deleted events
        tx.send(SyncCommand::FileModified(PathBuf::from("storm.txt")))
            .unwrap();
        tx.send(SyncCommand::FileDeleted(PathBuf::from("storm.txt")))
            .unwrap();
        tx.send(SyncCommand::FileModified(PathBuf::from("storm.txt")))
            .unwrap();

        // Wait for debounce window (1s + margin)
        std::thread::sleep(std::time::Duration::from_millis(1500));

        // The final state should be synced (FileModified won)
        assert!(dest.join("storm.txt").exists());
        assert_eq!(fs::read(dest.join("storm.txt")).unwrap(), b"storm data");
    }

    #[test]
    fn test_trigger_full_scan_skipped_offline() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("nonexistent_source");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source, dest.clone());
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let _handle = start_sync_worker(0, config, store, rx, None, source_online);

        // Send TriggerFullScan
        tx.send(SyncCommand::TriggerFullScan).unwrap();

        // Wait for processing
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Dest should remain empty — no sync occurred
        let entries: Vec<_> = fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "Destination should be empty when source is offline"
        );
    }

    #[test]
    fn test_nested_directory_deletion_archive() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        // Sync a nested file
        fs::write(source.join("subdir").join("deep.txt"), b"nested content").unwrap();
        engine.sync_file("subdir/deep.txt").unwrap();
        assert!(dest.join("subdir").join("deep.txt").exists());

        // Delete it
        engine.delete_file("subdir/deep.txt").unwrap();

        // Original should be gone
        assert!(!dest.join("subdir").join("deep.txt").exists());

        // Should be in .syncdir_archive with nested path preserved
        let archive = dest.join(".syncdir_archive");
        assert!(archive.exists());
        let entries: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let archived_entry = &entries[0];
        let nested = archived_entry.path().join("deep.txt");
        assert!(
            nested.exists(),
            "Archived nested file should preserve directory structure"
        );

        // DB record should be gone
        assert!(engine.db.get_file("subdir/deep.txt").unwrap().is_none());
    }

    #[test]
    fn test_full_scan_continues_past_file_errors() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        // Create a valid file in source
        fs::write(source.join("good.txt"), b"good content").unwrap();

        // Create a nested file in source under "bad/nested.txt"
        fs::create_dir_all(source.join("bad")).unwrap();
        fs::write(source.join("bad").join("nested.txt"), b"bad content").unwrap();

        // Create a file in destination at "dst/bad" so create_dir_all fails on "dst/bad/nested.txt"
        fs::write(dest.join("bad"), b"blocking file").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // Run full scan: should return PartialFailure because good.txt succeeded despite "bad/nested.txt" failing
        assert!(matches!(
            engine.run_full_scan().unwrap(),
            ScanOutcome::PartialFailure {
                synced: 1,
                failed: 1
            }
        ));

        // "good.txt" should be successfully synced
        assert!(dest.join("good.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("good.txt")).unwrap(),
            "good content"
        );

        // "bad/nested.txt" should not exist because creation failed
        assert!(!dest.join("bad").join("nested.txt").exists());
    }

    #[test]
    fn test_full_scan_all_skipped_returns_false() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        // Create a nested file in source under "bad/nested.txt"
        fs::create_dir_all(source.join("bad")).unwrap();
        fs::write(source.join("bad").join("nested.txt"), b"bad content").unwrap();

        // Create a file in destination at "dst/bad" so create_dir_all fails on "dst/bad/nested.txt"
        fs::write(dest.join("bad"), b"blocking file").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // Run full scan: 100% of files fail (1/1 file failed), so run_full_scan returns DestinationUnreachable
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
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // When dest_dir is missing, run_full_scan returns DestinationUnreachable immediately
        assert_eq!(
            engine.run_full_scan().unwrap(),
            ScanOutcome::DestinationUnreachable
        );
    }

    #[test]
    fn test_smb_timestamp_tolerance_fast_path() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let file_name = "test.txt";
        fs::write(source.join(file_name), b"test content").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // First sync
        engine.sync_file(file_name).unwrap();
        assert!(dest.join(file_name).exists());

        // Modify destination file timestamp slightly (1500 ms off) to simulate SMB rounding
        let dest_file = OpenOptions::new()
            .write(true)
            .open(dest.join(file_name))
            .unwrap();
        let src_meta = fs::metadata(source.join(file_name)).unwrap();
        let src_mtime = src_meta.modified().unwrap();
        let rounded_mtime = src_mtime - std::time::Duration::from_millis(1500);
        dest_file
            .set_times(fs::FileTimes::new().set_modified(rounded_mtime))
            .unwrap();

        // Second sync: should fast-path return Ok(()) due to ±2000 ms tolerance
        engine.sync_file(file_name).unwrap();
    }

    #[test]
    fn test_safe_epoch_duration_millis_positive() {
        assert_eq!(
            safe_epoch_duration_millis(1000),
            std::time::Duration::from_millis(1000)
        );
    }

    #[test]
    fn test_safe_epoch_duration_millis_negative() {
        assert_eq!(
            safe_epoch_duration_millis(-500),
            std::time::Duration::from_millis(0)
        );
    }
}

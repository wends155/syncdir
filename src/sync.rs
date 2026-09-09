//! Sync command types and engine trait for syncdir.
//!
//! Defines the message types that the monitor and tray threads
//! send to the sync worker, and the trait contract for the sync engine.

use crate::config::TargetSyncConfig;
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
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
    /// Some files failed to sync or delete.
    PartialFailure {
        synced: usize,
        failed: usize,
        delete_failed: usize,
    },
    /// Destination is unreachable.
    DestinationUnreachable,
}

/// Network and target connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityState {
    /// Connection is online and reachable.
    Online,
    /// Connection is offline or unreachable.
    Offline,
}

impl From<bool> for ConnectivityState {
    fn from(b: bool) -> Self {
        if b { Self::Online } else { Self::Offline }
    }
}

impl From<ConnectivityState> for bool {
    fn from(c: ConnectivityState) -> Self {
        matches!(c, ConnectivityState::Online)
    }
}

/// Directory watcher status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherState {
    /// Directory watcher is active.
    Active,
    /// Directory watcher is inactive.
    Inactive,
}

impl From<bool> for WatcherState {
    fn from(b: bool) -> Self {
        if b { Self::Active } else { Self::Inactive }
    }
}

impl From<WatcherState> for bool {
    fn from(w: WatcherState) -> Self {
        matches!(w, WatcherState::Active)
    }
}

/// Observer for sync worker status changes. Decouples sync from UI.
pub trait SyncStatusObserver: Send + Sync + 'static {
    fn on_target_status_change(&self, target_index: usize, state: ConnectivityState);
    /// Forward source directory connectivity and watcher active status to observers.
    fn on_watcher_status_change(&self, _source: ConnectivityState, _watcher: WatcherState) {}
    /// Notification when a file's write verification permanently fails after retries.
    fn on_write_verification_failed(&self, _path: &Path) {}
}

/// Contiguous range of dirty blocks to coalesce delta writes and reduce seek overhead.
#[derive(Debug)]
pub struct DirtyBlockRange {
    start_block: u64,
    block_count: u64,
    block_size: u64,
    data: Vec<u8>,
}

impl DirtyBlockRange {
    /// Maximum coalesced batch size in bytes (16MB).
    pub const MAX_COALESCE_BYTES: usize = 16 * 1024 * 1024;

    /// Create an empty dirty block range with pinned block size.
    pub fn new(block_size: u64) -> Self {
        Self {
            start_block: 0,
            block_count: 0,
            block_size,
            data: Vec::new(),
        }
    }

    /// Return the starting block index of this contiguous range.
    #[must_use]
    pub fn start_block(&self) -> u64 {
        self.start_block
    }

    /// Return the end block index (exclusive) of this contiguous range.
    #[must_use]
    pub fn end_block(&self) -> u64 {
        self.start_block + self.block_count
    }

    /// Return the number of contiguous blocks coalesced in this range.
    #[must_use]
    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Return the pinned block size in bytes for this range.
    #[must_use]
    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    /// Check whether the range is currently empty (contains 0 blocks).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.block_count == 0
    }

    /// Return the cumulative byte length of all coalesced blocks.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    /// Return the current allocated capacity of the underlying dirty buffer.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Return a read-only slice of the accumulated dirty block bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Append a block to the dirty range, automatically flushing to the writer if non-contiguous or full.
    ///
    /// # Arguments
    ///
    /// * `block_idx` - The zero-based index of the block.
    /// * `block_bytes` - Byte slice containing the modified block data.
    /// * `writer` - Target file stream implementing [`std::io::Write`] and [`std::io::Seek`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::SyncError::Io`] if flushing dirty blocks fails.
    pub fn add_block<W: std::io::Write + std::io::Seek>(
        &mut self,
        block_idx: u64,
        block_bytes: &[u8],
        writer: &mut W,
    ) -> Result<(), crate::error::SyncError> {
        let is_contiguous =
            self.block_count > 0 && block_idx == self.start_block + self.block_count;
        let fits = self.data.len() + block_bytes.len() <= Self::MAX_COALESCE_BYTES;
        if !is_contiguous || !fits {
            self.flush(writer)?;
            self.start_block = block_idx;
        }
        self.data.extend_from_slice(block_bytes);
        self.block_count += 1;
        Ok(())
    }

    /// Flush all buffered dirty blocks to the underlying writer stream at the coalesced offset.
    ///
    /// # Arguments
    ///
    /// * `writer` - Target file stream implementing [`std::io::Write`] and [`std::io::Seek`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::SyncError::Io`] if seeking or writing fails.
    pub fn flush<W: std::io::Write + std::io::Seek>(
        &mut self,
        writer: &mut W,
    ) -> Result<(), crate::error::SyncError> {
        if self.block_count > 0 {
            writer.seek(std::io::SeekFrom::Start(self.start_block * self.block_size))?;
            writer.write_all(&self.data)?;
            self.data.clear();
            self.block_count = 0;
        }
        Ok(())
    }

    /// Reset the dirty block range for reuse across files without reallocating buffer capacity.
    pub fn reset(&mut self) {
        self.start_block = 0;
        self.block_count = 0;
        self.data.clear();
    }
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
pub trait SyncEngine: Send + Sync {
    /// Synchronize a single file from source to destination.
    fn sync_file(&self, path: &Path) -> Result<(), SyncError>;
    /// Synchronize a single file with a reusable scratch buffer.
    fn sync_file_buffered(&self, path: &Path, _scratch: &mut [u8]) -> Result<(), SyncError> {
        self.sync_file(path)
    }
    /// Synchronize a file to a specific destination directory with a reusable scratch buffer.
    ///
    /// Required method: implementors must handle the `dest_dir` parameter.
    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError>;
    /// Handle deletion of a file (archive on destination).
    fn delete_file(&self, path: &Path) -> Result<(), SyncError>;
    /// Handle deletion of a file on a specific destination directory.
    ///
    /// Required method: implementors must handle the `dest_dir` parameter.
    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError>;
    /// Prune archive directory on the destination.
    fn prune_archive(&self, _dest_dir: &Path) -> Result<(), SyncError> {
        Ok(())
    }
    /// Perform a full directory scan and sync all changed files cooperatively cancellable.
    fn run_cancellable_full_scan(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError>;

    /// Perform a full directory scan and sync all changed files.
    ///
    /// Backward-compatible default delegates to static never-cancelled token.
    fn run_full_scan(&self) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        self.run_cancellable_full_scan(&NEVER_CANCELLED)
    }
}

type SyncErrorFactory = std::sync::Arc<dyn Fn() -> SyncError + Send + Sync>;
type SyncHandler = std::sync::Arc<dyn Fn(&Path) -> Result<(), SyncError> + Send + Sync>;

/// Thread-safe mock implementation of `SyncEngine` for isolated unit testing.
#[derive(Clone, Default)]
pub struct MockSyncEngine {
    synced_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
    deleted_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
    full_scans: std::sync::Arc<std::sync::Mutex<usize>>,
    sync_error_fn: std::sync::Arc<std::sync::Mutex<Option<SyncErrorFactory>>>,
    sync_handler: std::sync::Arc<std::sync::Mutex<Option<SyncHandler>>>,
    delete_handler: std::sync::Arc<std::sync::Mutex<Option<SyncHandler>>>,
    scan_outcome: std::sync::Arc<std::sync::Mutex<Option<ScanOutcome>>>,
}

impl std::fmt::Debug for MockSyncEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockSyncEngine")
            .field("synced_calls", &self.synced_calls)
            .field("deleted_calls", &self.deleted_calls)
            .field("full_scans", &self.full_scans)
            .field("scan_outcome", &self.scan_outcome)
            .finish()
    }
}

impl MockSyncEngine {
    /// Create a new empty mock sync engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an error factory to be returned by future sync and delete operations.
    pub fn set_sync_error(&self, error_fn: impl Fn() -> SyncError + Send + Sync + 'static) {
        let mut err = self.sync_error_fn.lock().unwrap();
        *err = Some(std::sync::Arc::new(error_fn));
    }

    /// Clear configured sync error factory.
    pub fn clear_sync_error(&self) {
        let mut err = self.sync_error_fn.lock().unwrap();
        *err = None;
    }

    /// Set a dynamic handler returning Result<(), SyncError> on sync calls.
    pub fn set_sync_handler(
        &self,
        handler: impl Fn(&Path) -> Result<(), SyncError> + Send + Sync + 'static,
    ) {
        let mut h = self.sync_handler.lock().unwrap();
        *h = Some(std::sync::Arc::new(handler));
    }

    /// Clear configured sync handler.
    pub fn clear_sync_handler(&self) {
        let mut h = self.sync_handler.lock().unwrap();
        *h = None;
    }

    /// Set a dynamic handler returning Result<(), SyncError> on delete calls.
    pub fn set_delete_handler(
        &self,
        handler: impl Fn(&Path) -> Result<(), SyncError> + Send + Sync + 'static,
    ) {
        let mut h = self.delete_handler.lock().unwrap();
        *h = Some(std::sync::Arc::new(handler));
    }

    /// Clear configured delete handler.
    pub fn clear_delete_handler(&self) {
        let mut h = self.delete_handler.lock().unwrap();
        *h = None;
    }

    /// Set the scan outcome to be returned by `run_full_scan`.
    pub fn set_scan_outcome(&self, outcome: Option<ScanOutcome>) {
        let mut sc = self.scan_outcome.lock().unwrap();
        *sc = outcome;
    }

    /// Return recorded (rel_path, dest_dir) tuples for `sync_file` calls.
    pub fn synced_calls(&self) -> Vec<(PathBuf, PathBuf)> {
        self.synced_calls.lock().unwrap().clone()
    }

    /// Return recorded (rel_path, dest_dir) tuples for `delete_file` calls.
    pub fn deleted_calls(&self) -> Vec<(PathBuf, PathBuf)> {
        self.deleted_calls.lock().unwrap().clone()
    }

    /// Return count of `run_full_scan` calls.
    pub fn full_scans_count(&self) -> usize {
        *self.full_scans.lock().unwrap()
    }
}

impl SyncEngine for MockSyncEngine {
    fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, Path::new(""), &mut [])
    }

    fn sync_file_buffered(&self, path: &Path, scratch: &mut [u8]) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, Path::new(""), scratch)
    }

    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        _scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let handler = self.sync_handler.lock().unwrap().clone();
        if let Some(h) = handler {
            h(path)?;
        } else {
            let err_fn = self.sync_error_fn.lock().unwrap().clone();
            if let Some(f) = err_fn {
                return Err(f());
            }
        }
        self.synced_calls
            .lock()
            .unwrap()
            .push((path.to_path_buf(), dest_dir.to_path_buf()));
        Ok(())
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, Path::new(""))
    }

    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        let handler = self.delete_handler.lock().unwrap().clone();
        if let Some(h) = handler {
            h(path)?;
        } else {
            let err_fn = self.sync_error_fn.lock().unwrap().clone();
            if let Some(f) = err_fn {
                return Err(f());
            }
        }
        self.deleted_calls
            .lock()
            .unwrap()
            .push((path.to_path_buf(), dest_dir.to_path_buf()));
        Ok(())
    }

    fn prune_archive(&self, _dest_dir: &Path) -> Result<(), SyncError> {
        Ok(())
    }

    fn run_cancellable_full_scan(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        *self.full_scans.lock().unwrap() += 1;
        let err_fn = self.sync_error_fn.lock().unwrap().clone();
        if let Some(f) = err_fn {
            return Err(f());
        }
        if let Some(outcome) = self.scan_outcome.lock().unwrap().clone() {
            return Ok(outcome);
        }
        Ok(ScanOutcome::Success { synced: 0 })
    }
}

fn hash_file_streamed(path: &Path) -> Result<blake3::Hash, SyncError> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65536];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(SyncError::Io(e)),
        }
    }
    Ok(hasher.finalize())
}

fn verify_small_file_write(src: &Path, dest: &Path) -> Result<(), SyncError> {
    let src_hash = hash_file_streamed(src)?;
    let dest_hash = hash_file_streamed(dest)?;
    if src_hash != dest_hash {
        return Err(SyncError::write_verification_failed(dest.to_path_buf()));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_destination_not_reparse(
    dest_dir: &Path,
    rel_path: &Path,
) -> Result<Option<std::fs::Metadata>, SyncError> {
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
fn verify_destination_not_reparse(
    dest_dir: &Path,
    rel_path: &Path,
) -> Result<Option<std::fs::Metadata>, SyncError> {
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
) -> Result<Option<std::fs::Metadata>, SyncError> {
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
) -> Result<Option<std::fs::Metadata>, SyncError> {
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
fn verify_source_not_reparse(source_dir: &Path, rel_path: &Path) -> Result<(), SyncError> {
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
fn verify_source_not_reparse(source_dir: &Path, rel_path: &Path) -> Result<(), SyncError> {
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
fn is_reparse_or_symlink_meta(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    (meta.file_attributes() & 0x400) != 0 || meta.file_type().is_symlink()
}

#[cfg(not(windows))]
fn is_reparse_or_symlink_meta(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

fn prune_archive(archive_dir: &Path, max_age_days: u64, max_bytes: u64) -> Result<(), SyncError> {
    if !archive_dir.exists() {
        return Ok(());
    }
    let max_age = std::time::Duration::from_secs(max_age_days * 24 * 3600);
    let now = SystemTime::now();

    let mut files = Vec::new();
    let mut total_bytes = 0u64;

    fn collect_files(
        dir: &Path,
        files: &mut Vec<(PathBuf, u64, SystemTime)>,
        total_bytes: &mut u64,
        depth: usize,
    ) -> std::io::Result<()> {
        const MAX_ARCHIVE_DEPTH: usize = 32;
        if depth > MAX_ARCHIVE_DEPTH {
            tracing::warn!(path = %dir.display(), "Max archive directory depth exceeded, skipping");
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if is_reparse_or_symlink(&entry) {
                tracing::debug!(path = %entry.path().display(), "Skipping reparse point or symlink in archive");
                continue;
            }
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_dir() {
                collect_files(&path, files, total_bytes, depth + 1)?;
            } else if ft.is_file()
                && let Ok(meta) = entry.metadata()
            {
                let len = meta.len();
                let mod_time = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                *total_bytes += len;
                files.push((path, len, mod_time));
            }
        }
        Ok(())
    }

    let _ = collect_files(archive_dir, &mut files, &mut total_bytes, 0);

    // Evict files older than max_age_days
    files.retain(|(path, len, mod_time)| {
        if let Ok(age) = now.duration_since(*mod_time)
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

/// Structured metadata snapshot for type-safe file comparison.
///
/// Prevents parameter transposition bugs in metadata comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMetadataSnapshot {
    /// File size in bytes.
    pub size: i64,
    /// Last modified time as milliseconds since UNIX epoch.
    pub modified_epoch_millis: i64,
}

impl FileMetadataSnapshot {
    /// Create a new metadata snapshot.
    pub fn new(size: i64, modified_epoch_millis: i64) -> Self {
        Self {
            size,
            modified_epoch_millis,
        }
    }
}

/// Raw metadata evaluation for testing and invariant assertion.
#[doc(hidden)]
pub fn is_metadata_up_to_date_raw(
    dest: &FileMetadataSnapshot,
    src: &FileMetadataSnapshot,
    record: Option<&crate::db::FileRecord>,
) -> bool {
    if let Some(record) = record
        && record.file_size == src.size
        && record.last_modified == src.modified_epoch_millis
        && dest.size == src.size
        && (dest.modified_epoch_millis - src.modified_epoch_millis).abs() <= 2000
    {
        return true;
    }
    false
}

/// Delta sync engine backed by a `HashStore` for signature caching.
pub struct LocalSyncEngine<S: HashStore> {
    pub(crate) db: S,
    pub(crate) config: TargetSyncConfig,
    pub(crate) resolved_dest: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct FileSyncTask<'a> {
    pub rel_path: &'a Path,
    pub src_path: &'a Path,
    pub dest_path: &'a Path,
    pub dest_dir: &'a Path,
    pub src_size: i64,
    pub src_mod: i64,
    pub cached_id: Option<i64>,
}

impl<S: HashStore> LocalSyncEngine<S> {
    /// Create a new sync engine with the given database and config.
    pub fn new(db: S, config: impl Into<TargetSyncConfig>) -> Self {
        Self {
            db,
            config: config.into(),
            resolved_dest: None,
        }
    }

    /// Set a pre-resolved destination path (e.g. from ReachabilityMonitor or worker context).
    pub fn with_resolved_dest(mut self, dest: impl Into<PathBuf>) -> Self {
        self.resolved_dest = Some(dest.into());
        self
    }

    /// Get the pre-resolved destination path if configured.
    pub fn resolved_dest(&self) -> Option<&Path> {
        self.resolved_dest.as_deref()
    }

    /// Build the archive path: `<dest>/.syncdir_archive/<ts>_<relative_path>`.
    fn get_archive_path(
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

    /// Synchronize a file or directory tree to a specific destination directory (primary or alternate).
    pub fn sync_file_to_dest(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        let mut scratch = vec![0u8; self.config.block_size_bytes() as usize];
        self.sync_file_to_dest_buffered(rel_path, dest_dir, &mut scratch)
    }

    fn is_metadata_up_to_date(
        dest_meta: Option<&std::fs::Metadata>,
        src_size: i64,
        src_mod: i64,
        rec: Option<&FileRecord>,
    ) -> bool {
        if let Some(dest_meta) = dest_meta {
            let dest = FileMetadataSnapshot {
                size: dest_meta.len() as i64,
                modified_epoch_millis: safe_modified_millis(dest_meta).unwrap_or(0),
            };
            let src = FileMetadataSnapshot {
                size: src_size,
                modified_epoch_millis: src_mod,
            };
            return is_metadata_up_to_date_raw(&dest, &src, rec);
        }
        false
    }

    fn sync_small_file(&self, task: &FileSyncTask<'_>) -> Result<(), SyncError> {
        if let Some(parent) = task.dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(task.src_path, task.dest_path)?;

        if self.config.verify_writes() {
            verify_small_file_write(task.src_path, task.dest_path)?;
        }

        let dest_file = OpenOptions::new().write(true).open(task.dest_path)?;
        dest_file.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + safe_epoch_duration_millis(task.src_mod)),
        )?;

        let record = FileRecord {
            id: task.cached_id,
            relative_path: task.rel_path.to_path_buf(),
            file_size: task.src_size,
            last_modified: task.src_mod,
        };
        self.db.save_file(&record, &[])?;
        tracing::info!(
            path = %task.rel_path.display(),
            target = %task.dest_dir.display(),
            size = task.src_size,
            "Synced file to destination"
        );
        Ok(())
    }

    pub(crate) fn sync_delta_large_file(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        if let Some(parent) = task.dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let dest_existed = task.dest_path.exists();
        let mut dest_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(task.dest_path)?;

        let dest_len = dest_file.metadata().map(|m| m.len()).unwrap_or(0);

        let old_hashes = if dest_existed && task.cached_id.is_some() {
            self.db.get_block_hashes(task.rel_path)?
        } else {
            Vec::new() // Force all blocks written if destination was deleted
        };

        let mut src_file = File::open(task.src_path)?;
        let block_size = self.config.block_size_bytes();
        let buf_size = block_size as usize;
        let mut heap_buf;
        let buffer: &mut [u8] = if scratch.len() >= buf_size {
            &mut scratch[..buf_size]
        } else {
            heap_buf = vec![0; buf_size];
            &mut heap_buf
        };
        let mut range = DirtyBlockRange::new(block_size);
        let mut modified_block_indices = Vec::new();
        let mut new_hashes = Vec::new();
        let mut block_idx = 0u64;
        let mut total_bytes_read = 0u64;

        loop {
            let bytes_read = read_block(&mut src_file, &mut *buffer)?;
            if bytes_read == 0 {
                break;
            }
            total_bytes_read += bytes_read as u64;
            let chunk = &buffer[..bytes_read];
            let hash = *blake3::hash(chunk).as_bytes();
            new_hashes.push(hash);

            let is_truncated_on_dest = dest_len < (block_idx * block_size + bytes_read as u64);
            let is_dirty = old_hashes.get(block_idx as usize) != Some(&hash);

            if is_dirty || is_truncated_on_dest {
                range.add_block(block_idx, chunk, &mut dest_file)?;
                if self.config.verify_writes() {
                    modified_block_indices.push((block_idx, bytes_read, hash));
                }
            }
            block_idx += 1;
        }
        range.flush(&mut dest_file)?;

        if self.config.verify_writes() {
            for &(b_idx, bytes_len, expected_hash) in &modified_block_indices {
                dest_file.seek(SeekFrom::Start(b_idx * block_size))?;
                dest_file.read_exact(&mut buffer[..bytes_len])?;
                let actual_hash = *blake3::hash(&buffer[..bytes_len]).as_bytes();
                if actual_hash != expected_hash {
                    return Err(SyncError::write_verification_failed_block(
                        task.dest_path.to_path_buf(),
                        b_idx,
                        expected_hash,
                        actual_hash,
                    ));
                }
            }
        }

        // Truncate to exact bytes read (TOCTOU protection)
        dest_file.set_len(total_bytes_read)?;
        dest_file.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + safe_epoch_duration_millis(task.src_mod)),
        )?;

        let record = FileRecord {
            id: task.cached_id,
            relative_path: task.rel_path.to_path_buf(),
            file_size: total_bytes_read as i64,
            last_modified: task.src_mod,
        };
        self.db.save_file(&record, &new_hashes)?;
        tracing::info!(
            path = %task.rel_path.display(),
            target = %task.dest_dir.display(),
            size = total_bytes_read,
            "Synced file to destination (delta)"
        );
        Ok(())
    }

    fn sync_file_to_dest_buffered_with_record(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
        file_record: Option<&FileRecord>,
    ) -> Result<(), SyncError> {
        if !is_safe_relative_path(rel_path) {
            return Err(SyncError::validation(format!(
                "Unsafe path traversal detected: {}",
                rel_path.display()
            )));
        }
        let src_path = self.config.source_dir().join(rel_path);
        let dest_path = dest_dir.join(rel_path);

        let sym_meta = fs::symlink_metadata(&src_path).map_err(SyncError::Io)?;
        if is_reparse_or_symlink_meta(&sym_meta) {
            tracing::debug!(path = %src_path.display(), "Skipping symlink or reparse point");
            return Ok(());
        }
        verify_source_not_reparse(self.config.source_dir(), rel_path)?;
        if sym_meta.is_dir() {
            let _ = verify_destination_not_reparse(dest_dir, rel_path)?;
            fs::create_dir_all(&dest_path)?;
            let mut dir_files = HashSet::new();
            let mut scan_complete = true;
            scan_dir(
                &src_path,
                self.config.source_dir(),
                &mut dir_files,
                &mut scan_complete,
                0,
            )?;
            for child_rel in &dir_files {
                self.sync_file_to_dest_buffered(child_rel, dest_dir, scratch)?;
            }
            return Ok(());
        }

        let dest_meta = verify_destination_not_reparse(dest_dir, rel_path)?;

        let src_size = sym_meta.len() as i64;
        let src_mod = safe_modified_millis(&sym_meta)?;

        if Self::is_metadata_up_to_date(dest_meta.as_ref(), src_size, src_mod, file_record) {
            tracing::debug!(path = %rel_path.display(), "Metadata unchanged, skipping sync");
            return Ok(());
        }

        let cached_id = file_record.and_then(|r| r.id);
        let task = FileSyncTask {
            rel_path,
            src_path: &src_path,
            dest_path: &dest_path,
            dest_dir,
            src_size,
            src_mod,
            cached_id,
        };

        if (src_size as u64) < self.config.block_sync_threshold_bytes {
            self.sync_small_file(&task)
        } else {
            self.sync_delta_large_file(&task, scratch)
        }
    }

    /// Synchronize a file or directory tree using a reusable scratch buffer.
    pub fn sync_file_to_dest_buffered(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let file_record = self.db.get_file(rel_path)?;
        self.sync_file_to_dest_buffered_with_record(
            rel_path,
            dest_dir,
            scratch,
            file_record.as_ref(),
        )
    }

    /// Handle deletion of a file on a specific destination directory.
    pub fn delete_file_from_dest(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        if !is_safe_relative_path(rel_path) {
            return Err(SyncError::validation(format!(
                "Unsafe path traversal detected: {}",
                rel_path.display()
            )));
        }
        let dest_path = dest_dir.join(rel_path);

        if !dest_dir.exists() {
            return Err(SyncError::Io(std::io::Error::from_raw_os_error(53)));
        }

        // Verify destination path does not traverse reparse points (Finding #11)
        verify_destination_not_reparse(dest_dir, rel_path)?;

        if dest_path.exists() && self.config.propagate_deletions() {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| {
                    SyncError::Io(std::io::Error::other(format!("System clock error: {e}")))
                })?
                .as_millis()
                .to_string();

            let mut archive_path = self.get_archive_path(dest_dir, rel_path, &timestamp)?;
            let mut counter = 1u32;
            while archive_path.exists() {
                archive_path = self.get_archive_path(
                    dest_dir,
                    rel_path,
                    &format!("{}_{}", timestamp, counter),
                )?;
                counter += 1;
            }

            if let Some(parent) = archive_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&dest_path, &archive_path)?;
        }
        self.db.delete_file(rel_path)?;
        Ok(())
    }

    /// Prune old and excess files in the destination archive.
    pub fn prune_destination_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        let archive_dir = dest_dir.join(".syncdir_archive");
        prune_archive(&archive_dir, 30, 10 * 1024 * 1024 * 1024)
    }
}

impl<S: HashStore> SyncEngine for LocalSyncEngine<S> {
    fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        self.sync_file_to_dest(path, self.config.dest_dir())
    }

    fn sync_file_buffered(&self, path: &Path, scratch: &mut [u8]) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, self.config.dest_dir(), scratch)
    }

    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, dest_dir, scratch)
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, self.config.dest_dir())
    }

    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, dest_dir)
    }

    fn prune_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        self.prune_destination_archive(dest_dir)
    }

    fn run_cancellable_full_scan(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }

        let resolved_source = self.config.source_dir();
        if !resolved_source.exists() {
            return Err(SyncError::validation("Source directory does not exist"));
        }

        let dest = self.config.dest_dir();
        let dest_reachable = dest.exists() && dest.is_dir();
        let (active_dest, is_reachable) = if let Some(ref pre_resolved) = self.resolved_dest {
            if pre_resolved.exists() && pre_resolved.is_dir() {
                (pre_resolved.clone(), true)
            } else {
                (dest.to_path_buf(), false)
            }
        } else if !dest_reachable {
            let alt_path = crate::net::try_resolve_alternate_path(dest);
            if alt_path.exists() && alt_path.is_dir() {
                if alt_path != *dest {
                    tracing::info!(
                        target = %dest.display(),
                        resolved_path = %alt_path.display(),
                        "Target destination resolved alternate mapped drive/UNC SMB path for full scan."
                    );
                }
                (alt_path, true)
            } else {
                (dest.to_path_buf(), false)
            }
        } else {
            (dest.to_path_buf(), true)
        };

        if !is_reachable {
            tracing::warn!(
                target = %dest.display(),
                "Target destination directory does not exist or is unreachable. Skipping full scan."
            );
            return Ok(ScanOutcome::DestinationUnreachable);
        }

        let mut source_files: HashSet<PathBuf> = HashSet::new();
        let mut scan_complete = true;
        scan_dir_cancellable(
            resolved_source,
            resolved_source,
            &mut source_files,
            &mut scan_complete,
            0,
            cancel,
        )?;

        let cached_records = self.db.list_all_records()?;

        // Sync all source files
        let mut synced_count = 0usize;
        let mut failed_count = 0usize;
        let mut sync_skip_count = 0usize;
        let mut scratch = vec![0u8; self.config.block_size_bytes() as usize];
        for rel_path in &source_files {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(SyncError::Cancelled);
            }
            let normalized_key = PathBuf::from(rel_path.to_string_lossy().replace('\\', "/"));
            match self.sync_file_to_dest_buffered_with_record(
                rel_path,
                &active_dest,
                &mut scratch,
                cached_records.get(&normalized_key),
            ) {
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

                for tracked_path in cached_records.keys() {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
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

                    if !is_present
                        && let Err(e) = self.delete_file_from_dest(tracked_path, &active_dest)
                    {
                        let os_code = match &e {
                            SyncError::Io(io_err) => io_err.raw_os_error(),
                            _ => None,
                        };
                        tracing::warn!(
                            path = %tracked_path.display(),
                            target = %active_dest.display(),
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
                        target = %active_dest.display(),
                        "Full scan completed with deletion errors"
                    );
                }
            }
        }

        let _ = self.prune_destination_archive(&active_dest);

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

#[cfg(windows)]
fn is_reparse_or_symlink(entry: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    match fs::symlink_metadata(entry.path()) {
        Ok(meta) => (meta.file_attributes() & 0x400) != 0 || meta.file_type().is_symlink(),
        Err(_) => true,
    }
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(true)
}

fn scan_dir_cancellable(
    dir: &Path,
    source_root: &Path,
    files: &mut HashSet<PathBuf>,
    scan_complete: &mut bool,
    depth: usize,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), SyncError> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
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
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
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
        if is_reparse_or_symlink(&entry) {
            tracing::debug!(path = %entry.path().display(), "Skipping reparse point or symlink in scan");
            continue;
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
    static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
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

/// Thread-safe tracker for source directory connectivity.
#[derive(Clone, Debug)]
pub struct SourceConnectivityTracker(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl SourceConnectivityTracker {
    /// Create a new tracker with initial online state.
    pub fn new(initial: bool) -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            initial,
        )))
    }

    /// Return true if the source is currently marked online.
    pub fn is_online(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set the source online status.
    pub fn set_online(&self, online: bool) {
        self.0.store(online, std::sync::atomic::Ordering::Relaxed);
    }

    /// Access the underlying `Arc<AtomicBool>` for low-level compatibility.
    pub fn raw_arc(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.0.clone()
    }
}

impl From<std::sync::Arc<std::sync::atomic::AtomicBool>> for SourceConnectivityTracker {
    fn from(arc: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self(arc)
    }
}

impl From<bool> for SourceConnectivityTracker {
    fn from(b: bool) -> Self {
        Self::new(b)
    }
}

/// Manages pending sync and delete paths with per-path debounce deadlines and capacity limits.
#[derive(Debug)]
pub struct DebounceQueue {
    pending_syncs: HashMap<PathBuf, Instant>,
    pending_deletes: HashMap<PathBuf, Instant>,
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(Instant, PathBuf)>>,
    max_capacity: usize,
}

impl DebounceQueue {
    /// Create a new debounce queue with given capacity.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            pending_syncs: HashMap::new(),
            pending_deletes: HashMap::new(),
            heap: std::collections::BinaryHeap::new(),
            max_capacity,
        }
    }

    /// Enqueue a path for sync with a debounce duration.
    /// If the path is already pending, its deadline is extended.
    /// Returns false if at capacity and the path was not already pending.
    pub fn enqueue_sync(&mut self, path: PathBuf, debounce: std::time::Duration) -> bool {
        if !self.pending_syncs.contains_key(&path)
            && self.pending_syncs.len() + self.pending_deletes.len() >= self.max_capacity
        {
            return false;
        }
        self.pending_deletes.remove(&path);
        let dl = Instant::now() + debounce;
        self.pending_syncs.insert(path.clone(), dl);
        self.heap.push(std::cmp::Reverse((dl, path)));
        true
    }

    /// Enqueue a path for deletion with a debounce duration.
    /// If the path is already pending, its deadline is extended.
    /// Returns false if at capacity and the path was not already pending.
    pub fn enqueue_delete(&mut self, path: PathBuf, debounce: std::time::Duration) -> bool {
        if !self.pending_deletes.contains_key(&path)
            && self.pending_syncs.len() + self.pending_deletes.len() >= self.max_capacity
        {
            return false;
        }
        self.pending_syncs.remove(&path);
        let dl = Instant::now() + debounce;
        self.pending_deletes.insert(path.clone(), dl);
        self.heap.push(std::cmp::Reverse((dl, path)));
        true
    }

    /// Drain and return all sync paths whose debounce deadlines are <= `now`.
    pub fn drain_ready_syncs(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        self.pending_syncs.retain(|path, deadline| {
            if *deadline <= now {
                ready.push(path.clone());
                false
            } else {
                true
            }
        });
        ready
    }

    /// Drain and return all delete paths whose debounce deadlines are <= `now`.
    pub fn drain_ready_deletes(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        self.pending_deletes.retain(|path, deadline| {
            if *deadline <= now {
                ready.push(path.clone());
                false
            } else {
                true
            }
        });
        ready
    }

    /// Re-enqueue a failed sync path for retry with a backoff delay.
    pub fn requeue_sync_retry(&mut self, path: PathBuf, delay: std::time::Duration) {
        let dl = Instant::now() + delay;
        self.pending_syncs.insert(path.clone(), dl);
        self.heap.push(std::cmp::Reverse((dl, path)));
    }

    /// Re-enqueue a failed delete path for retry with a backoff delay.
    pub fn requeue_delete_retry(&mut self, path: PathBuf, delay: std::time::Duration) {
        let dl = Instant::now() + delay;
        self.pending_deletes.insert(path.clone(), dl);
        self.heap.push(std::cmp::Reverse((dl, path)));
    }

    /// Calculate earliest deadline across all pending syncs and deletes.
    ///
    /// Uses min-heap top with lazy eviction of stale entries (whose deadline
    /// was updated or path was drained).
    pub fn earliest_deadline(&mut self) -> Option<Instant> {
        while let Some(std::cmp::Reverse((deadline, path))) = self.heap.peek() {
            let actual_deadline = self
                .pending_syncs
                .get(path)
                .or_else(|| self.pending_deletes.get(path));
            match actual_deadline {
                Some(&d) if d == *deadline => return Some(*deadline),
                _ => {
                    self.heap.pop();
                }
            }
        }
        None
    }

    /// Return true if both pending sync and delete queues are empty.
    pub fn is_empty(&self) -> bool {
        self.pending_syncs.is_empty() && self.pending_deletes.is_empty()
    }

    /// Return count of pending syncs.
    pub fn pending_sync_count(&self) -> usize {
        self.pending_syncs.len()
    }

    /// Return count of pending deletes.
    pub fn pending_delete_count(&self) -> usize {
        self.pending_deletes.len()
    }

    /// Return total count of pending syncs and deletes.
    pub fn pending_count(&self) -> usize {
        self.pending_syncs.len() + self.pending_deletes.len()
    }
}

/// Manages reachability checks and alternate network path resolution for a target worker.
pub struct ReachabilityMonitor {
    target_index: usize,
    configured_dest: PathBuf,
    active_dest: PathBuf,
    dest_online: bool,
    last_sent_status: Option<ConnectivityState>,
    last_status_check: Option<Instant>,
    retry_dur: std::time::Duration,
    resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
}

impl ReachabilityMonitor {
    /// Create a new reachability monitor for a target directory.
    pub fn new(
        target_index: usize,
        configured_dest: PathBuf,
        retry_interval_seconds: u64,
        resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
    ) -> Self {
        let active_dest = configured_dest.clone();
        Self {
            target_index,
            configured_dest,
            active_dest,
            dest_online: false,
            last_sent_status: None,
            last_status_check: None,
            retry_dur: std::time::Duration::from_secs(retry_interval_seconds),
            resolver,
        }
    }

    /// Return the active resolved destination directory path.
    pub fn active_dest(&self) -> &Path {
        &self.active_dest
    }

    /// Return true if the destination is currently determined to be online.
    pub fn is_dest_online(&self) -> bool {
        self.dest_online
    }

    /// Check if enough time has elapsed to warrant another reachability check.
    pub fn should_check_reachability(&self, now: Instant) -> bool {
        match self.last_status_check {
            None => true,
            Some(last) => now.duration_since(last) >= self.retry_dur,
        }
    }

    /// Probe destination reachability and resolve alternate UNC paths if necessary.
    pub fn check_reachability(
        &mut self,
        now: Instant,
        observer: Option<&std::sync::Arc<dyn SyncStatusObserver>>,
    ) {
        self.last_status_check = Some(now);

        let resolved = self
            .resolver
            .try_resolve_alternate_path(&self.configured_dest);
        let online = match std::fs::metadata(&resolved) {
            Ok(m) => m.is_dir(),
            Err(_) => match std::fs::metadata(&self.configured_dest) {
                Ok(m) => m.is_dir(),
                Err(_) => false,
            },
        };

        if online {
            if std::fs::metadata(&resolved)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                self.active_dest = resolved;
            } else {
                self.active_dest = self.configured_dest.clone();
            }
            self.dest_online = true;
        } else {
            self.active_dest = self.configured_dest.clone();
            self.dest_online = false;
        }

        let new_state = if self.dest_online {
            ConnectivityState::Online
        } else {
            ConnectivityState::Offline
        };

        if self.last_sent_status != Some(new_state) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, new_state);
            }
            self.last_sent_status = Some(new_state);
        }
    }

    /// Mark the target destination offline immediately and notify observers.
    pub fn mark_offline(&mut self, observer: Option<&std::sync::Arc<dyn SyncStatusObserver>>) {
        self.dest_online = false;
        if self.last_sent_status != Some(ConnectivityState::Offline) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, ConnectivityState::Offline);
            }
            self.last_sent_status = Some(ConnectivityState::Offline);
        }
    }

    /// Mark the target destination online immediately and notify observers.
    pub fn mark_online(&mut self, observer: Option<&std::sync::Arc<dyn SyncStatusObserver>>) {
        self.dest_online = true;
        if self.last_sent_status != Some(ConnectivityState::Online) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, ConnectivityState::Online);
            }
            self.last_sent_status = Some(ConnectivityState::Online);
        }
    }
}

/// State container for the sync worker execution loop.
pub struct SyncWorkerState {
    pub scratch: Vec<u8>,
    pub failure_tracker: HashMap<PathBuf, u32>,
    pub hourly_prune_interval: std::time::Duration,
    pub last_archive_prune: Instant,
}

impl SyncWorkerState {
    /// Initialize worker scratch buffer and archive prune timers.
    pub fn new(block_size_bytes: u64) -> Self {
        Self {
            scratch: vec![0u8; block_size_bytes as usize],
            failure_tracker: HashMap::new(),
            hourly_prune_interval: std::time::Duration::from_secs(3600),
            last_archive_prune: Instant::now(),
        }
    }

    /// Check if enough time has passed to trigger the hourly archive prune.
    pub fn should_prune_archive(&self, now: Instant) -> bool {
        now.duration_since(self.last_archive_prune) >= self.hourly_prune_interval
    }

    /// Record timestamp of the most recent archive pruning.
    pub fn record_prune(&mut self, now: Instant) {
        self.last_archive_prune = now;
    }

    /// Increment and return consecutive failure count for a path.
    pub fn record_failure(&mut self, path: &Path) -> u32 {
        let entry = self.failure_tracker.entry(path.to_path_buf()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Reset failure count upon successful synchronization.
    pub fn reset_failure(&mut self, path: &Path) {
        self.failure_tracker.remove(path);
    }
}

/// Execution context for a target synchronization worker thread.
pub struct SyncWorkerContext<E: SyncEngine> {
    pub target_index: usize,
    pub config: TargetSyncConfig,
    pub engine: E,
    pub rx: std::sync::mpsc::Receiver<SyncCommand>,
    pub observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
    pub source_connectivity: SourceConnectivityTracker,
    pub resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
    pub cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub max_pending_queue: usize,
}

impl<E: SyncEngine> SyncWorkerContext<E> {
    /// Create a new sync worker context.
    pub fn new(
        target_index: usize,
        config: impl Into<TargetSyncConfig>,
        engine: E,
        rx: std::sync::mpsc::Receiver<SyncCommand>,
        observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
        source_connectivity: impl Into<SourceConnectivityTracker>,
    ) -> Self {
        Self {
            target_index,
            config: config.into(),
            engine,
            rx,
            observer,
            source_connectivity: source_connectivity.into(),
            resolver: std::sync::Arc::new(crate::net::Win32NetworkResolver),
            cancellation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            max_pending_queue: 50_000,
        }
    }

    /// Set a custom maximum pending queue capacity.
    pub fn with_max_pending_queue(mut self, max_pending_queue: usize) -> Self {
        self.max_pending_queue = max_pending_queue;
        self
    }

    /// Set a custom network resolver.
    pub fn with_resolver(
        mut self,
        resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
    ) -> Self {
        self.resolver = resolver;
        self
    }

    /// Set a custom cancellation token.
    pub fn with_cancellation(
        mut self,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Backwards-compatible accessor for raw atomic bool.
    pub fn source_online_atomic(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.source_connectivity.raw_arc()
    }
}

/// Calculates exponential backoff duration based on the number of attempts.
pub fn calculate_exponential_backoff(
    attempts: u32,
    base_interval: std::time::Duration,
) -> std::time::Duration {
    let factor = 2u64.saturating_pow(attempts.saturating_sub(1));
    let max_delay = std::time::Duration::from_secs(300);
    base_interval
        .checked_mul(factor.min(u32::MAX as u64) as u32)
        .unwrap_or(max_delay)
        .min(max_delay)
}

/// Spawns a background synchronization worker thread.
///
/// The worker listens for filesystem events (file changes/deletions) on its channel
/// and triggers block-level sync operations to its specific destination directory.
///
/// # Arguments
///
/// * `context` - Worker execution context containing the engine, configuration, channels, and observers.
///
/// # Returns
///
/// Returns the join handle for the spawned background worker thread, or a `SyncError` if spawning fails.
#[must_use = "dropping the JoinHandle detaches the sync worker thread"]
pub fn start_sync_worker<E: SyncEngine + 'static>(
    context: SyncWorkerContext<E>,
) -> Result<std::thread::JoinHandle<()>, SyncError> {
    let SyncWorkerContext {
        target_index,
        config,
        engine,
        rx,
        observer,
        source_connectivity,
        resolver,
        cancellation,
        max_pending_queue,
    } = context;

    std::thread::Builder::new()
        .name(format!("sync-worker-{}", target_index))
        .spawn(move || {
            let mut queue = DebounceQueue::new(max_pending_queue);
            let mut reachability = ReachabilityMonitor::new(
                target_index,
                config.dest_dir().to_path_buf(),
                config.retry_interval_seconds(),
                resolver,
            );
            let mut state = SyncWorkerState::new(config.block_size_bytes());
            let debounce_dur = std::time::Duration::from_secs(config.debounce_seconds());
            let retry_dur = std::time::Duration::from_secs(config.retry_interval_seconds());
            let mut needs_catchup_scan = false;
            let drain_threshold = 1_000.min(max_pending_queue / 2);

            loop {
                if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                    tracing::info!(
                        target_index = target_index + 1,
                        "Sync worker shutting down via cancellation signal."
                    );
                    break;
                }
                let now = Instant::now();

                let was_offline = !reachability.is_dest_online();
                if reachability.should_check_reachability(now) {
                    reachability.check_reachability(now, observer.as_ref());
                    if was_offline && reachability.is_dest_online() {
                        tracing::info!(
                            target_index = target_index + 1,
                            target_path = %config.dest_dir().display(),
                            "Target destination is back online."
                        );
                        if source_connectivity.is_online() {
                            tracing::info!(
                                target_index = target_index + 1,
                                "Triggering catch-up full scan following destination reconnect."
                            );
                            match engine.run_cancellable_full_scan(&cancellation) {
                                Ok(ScanOutcome::DestinationUnreachable) => {
                                    tracing::warn!(
                                        "Catch-up scan determined destination is unreachable"
                                    );
                                    reachability.mark_offline(observer.as_ref());
                                }
                                Ok(_) => {}
                                Err(SyncError::Cancelled) => {
                                    tracing::info!("Catch-up scan cancelled");
                                    break;
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Catch-up full scan on reconnect failed");
                                }
                            }
                        }
                    }
                }

                let timeout = match queue.earliest_deadline() {
                    Some(dl) if dl > now => (dl - now).min(std::time::Duration::from_secs(1)),
                    Some(_) => std::time::Duration::ZERO,
                    None => std::time::Duration::from_secs(1),
                };

                let mut cmd_opt = match rx.recv_timeout(timeout) {
                    Ok(cmd) => Some(cmd),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };

                while let Some(cmd) = cmd_opt {
                    match cmd {
                        SyncCommand::FileModified(path) => {
                            if !queue.enqueue_sync(path.clone(), debounce_dur) {
                                tracing::error!(
                                    path = %path.display(),
                                    target_index = target_index + 1,
                                    "Debounce queue overflow on file modify; scheduling catchup scan"
                                );
                                needs_catchup_scan = true;
                            }
                        }
                        SyncCommand::FileDeleted(path) => {
                            if !queue.enqueue_delete(path.clone(), debounce_dur) {
                                tracing::error!(
                                    path = %path.display(),
                                    target_index = target_index + 1,
                                    "Debounce queue overflow on file delete; scheduling catchup scan"
                                );
                                needs_catchup_scan = true;
                            }
                        }
                        SyncCommand::TriggerFullScan => {
                            if source_connectivity.is_online() {
                                match engine.run_cancellable_full_scan(&cancellation) {
                                    Ok(ScanOutcome::Success { synced }) => {
                                        tracing::info!(synced, "Full scan completed successfully");
                                        reachability.mark_online(observer.as_ref());
                                    }
                                    Ok(ScanOutcome::PartialFailure {
                                        synced,
                                        failed,
                                        delete_failed,
                                    }) => {
                                        tracing::warn!(
                                            synced,
                                            failed,
                                            delete_failed,
                                            target_path = %reachability.active_dest().display(),
                                            "Full scan completed with partial sync failures"
                                        );
                                        reachability.mark_online(observer.as_ref());
                                    }
                                    Ok(ScanOutcome::DestinationUnreachable) => {
                                        tracing::warn!(
                                            target_path = %reachability.active_dest().display(),
                                            "Full scan aborted: destination is unreachable"
                                        );
                                        reachability.mark_offline(observer.as_ref());
                                    }
                                    Err(SyncError::Cancelled) => {
                                        tracing::info!("Full scan cancelled");
                                        break;
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "Full scan execution failed");
                                    }
                                }
                            } else {
                                tracing::warn!("Skipping full scan: source directory is offline");
                            }
                        }
                    }
                    cmd_opt = rx.try_recv().ok();
                }

                let now = Instant::now();

                // Periodic archive pruning if destination is online
                if reachability.is_dest_online() && state.should_prune_archive(now) {
                    state.record_prune(now);
                    let _ = engine.prune_archive(reachability.active_dest());
                }

                let mut network_offline_detected = false;

                // Drain and process ready syncs
                let ready_syncs = queue.drain_ready_syncs(now);
                for path in ready_syncs {
                    if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if network_offline_detected
                        || !source_connectivity.is_online()
                        || !reachability.is_dest_online()
                    {
                        queue.requeue_sync_retry(path, retry_dur);
                        continue;
                    }

                    match engine.sync_file_to_dest_buffered(
                        &path,
                        reachability.active_dest(),
                        &mut state.scratch,
                    ) {
                        Ok(()) => {
                            state.reset_failure(&path);
                        }
                        Err(SyncError::WriteVerificationFailed { .. }) => {
                            let attempts = state.record_failure(&path);
                            if attempts <= 10 {
                                let backoff = calculate_exponential_backoff(attempts, retry_dur);
                                tracing::warn!(
                                    path = %path.display(),
                                    attempt = attempts,
                                    ?backoff,
                                    "Write verification failed; rescheduling retry"
                                );
                                queue.requeue_sync_retry(path, backoff);
                            } else {
                                tracing::error!(
                                    path = %path.display(),
                                    "Write verification permanently failed after 10 retries"
                                );
                                if let Some(ref obs) = observer {
                                    obs.on_write_verification_failed(&path);
                                }
                            }
                        }
                        Err(SyncError::Validation(msg)) => {
                            tracing::error!(
                                path = %path.display(),
                                target = %reachability.active_dest().display(),
                                error = %msg,
                                "Permanent validation failure; evicting from sync queue without retry"
                            );
                        }
                        Err(e) if e.is_network_offline() => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "Target offline detected; bailing out queue"
                            );
                            network_offline_detected = true;
                            reachability.mark_offline(observer.as_ref());
                            queue.requeue_sync_retry(path, retry_dur);
                        }
                        Err(e) => {
                            let os_code = match &e {
                                SyncError::Io(io_err) => io_err.raw_os_error(),
                                _ => None,
                            };
                            let attempts = state.record_failure(&path);
                            if attempts <= 10 {
                                let backoff = calculate_exponential_backoff(attempts, retry_dur);
                                tracing::warn!(
                                    path = %path.display(),
                                    target = %reachability.active_dest().display(),
                                    error = %e,
                                    os_error = ?os_code,
                                    attempt = attempts,
                                    ?backoff,
                                    "Sync failed, scheduling retry with backoff"
                                );
                                queue.requeue_sync_retry(path, backoff);
                            } else {
                                tracing::error!(
                                    path = %path.display(),
                                    target = %reachability.active_dest().display(),
                                    error = %e,
                                    os_error = ?os_code,
                                    "Sync permanently failed after 10 retries; evicting"
                                );
                            }
                        }
                    }
                }

                // Drain and process ready deletes
                let ready_deletes = queue.drain_ready_deletes(now);
                for path in ready_deletes {
                    if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if network_offline_detected
                        || !source_connectivity.is_online()
                        || !reachability.is_dest_online()
                    {
                        queue.requeue_delete_retry(path, retry_dur);
                        continue;
                    }

                    match engine.delete_file_from_dest(&path, reachability.active_dest()) {
                        Ok(()) => {
                            state.reset_failure(&path);
                        }
                        Err(SyncError::Validation(msg)) => {
                            tracing::error!(
                                path = %path.display(),
                                target = %reachability.active_dest().display(),
                                error = %msg,
                                "Permanent validation failure; evicting from deletion queue without retry"
                            );
                        }
                        Err(e) if e.is_network_offline() => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "Target offline detected during deletion; bailing out queue"
                            );
                            network_offline_detected = true;
                            reachability.mark_offline(observer.as_ref());
                            queue.requeue_delete_retry(path, retry_dur);
                        }
                        Err(e) => {
                            let os_code = match &e {
                                SyncError::Io(io_err) => io_err.raw_os_error(),
                                _ => None,
                            };
                            let attempts = state.record_failure(&path);
                            if attempts <= 10 {
                                let backoff = calculate_exponential_backoff(attempts, retry_dur);
                                tracing::warn!(
                                    path = %path.display(),
                                    target = %reachability.active_dest().display(),
                                    error = %e,
                                    os_error = ?os_code,
                                    attempt = attempts,
                                    ?backoff,
                                    "Deletion failed, scheduling retry with backoff"
                                );
                                queue.requeue_delete_retry(path, backoff);
                            } else {
                                tracing::error!(
                                    path = %path.display(),
                                    target = %reachability.active_dest().display(),
                                    error = %e,
                                    os_error = ?os_code,
                                    "Deletion permanently failed after 10 retries; evicting"
                                );
                            }
                        }
                    }
                }

                if needs_catchup_scan
                    && queue.pending_count() <= drain_threshold
                    && source_connectivity.is_online()
                    && reachability.is_dest_online()
                {
                    tracing::info!(
                        target_index = target_index + 1,
                        "Triggering catch-up full scan following queue overflow recovery"
                    );
                    match engine.run_cancellable_full_scan(&cancellation) {
                        Ok(ScanOutcome::Success { .. }) | Ok(ScanOutcome::PartialFailure { .. }) => {
                            needs_catchup_scan = false;
                        }
                        Ok(ScanOutcome::DestinationUnreachable) => {
                            reachability.mark_offline(observer.as_ref());
                        }
                        Err(SyncError::Cancelled) => {
                            break;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Catch-up scan after queue overflow failed");
                        }
                    }
                }
            }
        })
        .map_err(SyncError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::{BlockHash, MockHashStore, SqliteHashStore};
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
    fn test_is_metadata_up_to_date_raw() {
        let record = crate::db::FileRecord::new(PathBuf::from("file.txt"), 100, 10_000).with_id(1);
        let snap = |size, millis| FileMetadataSnapshot::new(size, millis);

        // Exact match
        assert!(is_metadata_up_to_date_raw(
            &snap(100, 10_000),
            &snap(100, 10_000),
            Some(&record)
        ));

        // Within 2000ms SMB tolerance
        assert!(is_metadata_up_to_date_raw(
            &snap(100, 11_500),
            &snap(100, 10_000),
            Some(&record)
        ));
        assert!(is_metadata_up_to_date_raw(
            &snap(100, 8_500),
            &snap(100, 10_000),
            Some(&record)
        ));

        // Beyond 2000ms tolerance
        assert!(!is_metadata_up_to_date_raw(
            &snap(100, 12_500),
            &snap(100, 10_000),
            Some(&record)
        ));
        assert!(!is_metadata_up_to_date_raw(
            &snap(100, 7_500),
            &snap(100, 10_000),
            Some(&record)
        ));

        // Size mismatch
        assert!(!is_metadata_up_to_date_raw(
            &snap(101, 10_000),
            &snap(100, 10_000),
            Some(&record)
        ));

        // No record
        assert!(!is_metadata_up_to_date_raw(
            &snap(100, 10_000),
            &snap(100, 10_000),
            None
        ));
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
        engine.sync_file(Path::new("tiny.txt")).unwrap();

        let content = fs::read_to_string(dest.join("tiny.txt")).unwrap();
        assert_eq!(content, "hi");

        // DB should have a record
        assert!(engine.db.get_file(Path::new("tiny.txt")).unwrap().is_some());
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
        engine.sync_file(Path::new("big.bin")).unwrap();

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

        engine.sync_file(Path::new("big.bin")).unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAAZZZZCCCC");

        // DB should have updated hashes
        let hashes = engine.db.get_block_hashes(Path::new("big.bin")).unwrap();
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
        engine.sync_file(Path::new("doomed.txt")).unwrap();
        assert!(dest.join("doomed.txt").exists());

        // Delete it
        engine.delete_file(Path::new("doomed.txt")).unwrap();

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
        assert!(
            engine
                .db
                .get_file(Path::new("doomed.txt"))
                .unwrap()
                .is_none()
        );
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
        engine.sync_file(Path::new("empty.txt")).unwrap();

        assert!(dest.join("empty.txt").exists());
        assert_eq!(fs::read(dest.join("empty.txt")).unwrap(), b"");
        assert!(
            engine
                .db
                .get_file(Path::new("empty.txt"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_exact_block_multiple_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(4)
            .block_size_bytes(4)
            .build();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // 8 bytes payload = exactly 2 blocks of 4 bytes
        fs::write(source.join("exact.bin"), b"12345678").unwrap();
        engine.sync_file(Path::new("exact.bin")).unwrap();

        assert_eq!(fs::read(dest.join("exact.bin")).unwrap(), b"12345678");
        let hashes = engine.db.get_block_hashes(Path::new("exact.bin")).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn test_worker_queue_debouncing_storm() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        // Write initial source file
        fs::write(source.join("storm.txt"), b"initial").unwrap();

        let engine = LocalSyncEngine::new(store, config.clone());
        let context = SyncWorkerContext::new(0, config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        // Allow initial scan to complete
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Update source file and send rapid burst of interleaved modified/deleted events
        fs::write(source.join("storm.txt"), b"storm data").unwrap();

        // Send rapid burst of interleaved modified/deleted events
        tx.send(SyncCommand::FileModified(PathBuf::from("storm.txt")))
            .unwrap();
        tx.send(SyncCommand::FileDeleted(PathBuf::from("storm.txt")))
            .unwrap();
        tx.send(SyncCommand::FileModified(PathBuf::from("storm.txt")))
            .unwrap();

        // Wait for debounce and sync to complete (debounce is 1s, allow up to 5s under load)
        let start = std::time::Instant::now();
        let mut synced = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(content) = fs::read(dest.join("storm.txt"))
                && content == b"storm data"
            {
                synced = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            synced,
            "Timed out waiting for debounced storm sync to complete"
        );
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
        let engine = LocalSyncEngine::new(store, config.clone());
        let context = SyncWorkerContext::new(0, config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

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
        engine.sync_file(Path::new("subdir/deep.txt")).unwrap();
        assert!(dest.join("subdir").join("deep.txt").exists());

        // Delete it
        engine.delete_file(Path::new("subdir/deep.txt")).unwrap();

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
        assert!(
            engine
                .db
                .get_file(Path::new("subdir/deep.txt"))
                .unwrap()
                .is_none()
        );
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
                failed: 1,
                delete_failed: 0,
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
        engine.sync_file(Path::new(file_name)).unwrap();
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
        engine.sync_file(Path::new(file_name)).unwrap();
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

    #[test]
    fn test_dirty_block_range_coalescing() {
        use std::io::Cursor;
        let mut cursor = Cursor::new(vec![0u8; 32]);
        let mut range = DirtyBlockRange::new(4);

        // Add contiguous blocks: block 0 (4 bytes), block 1 (4 bytes)
        range.add_block(0, b"AAAA", &mut cursor).unwrap();
        assert_eq!(range.block_count(), 1);
        assert_eq!(range.start_block(), 0);

        range.add_block(1, b"BBBB", &mut cursor).unwrap();
        assert_eq!(range.block_count(), 2);
        assert_eq!(range.start_block(), 0);

        // Add non-contiguous block 4 (should flush blocks 0-1 and start block 4)
        range.add_block(4, b"EEEE", &mut cursor).unwrap();
        assert_eq!(range.block_count(), 1);
        assert_eq!(range.start_block(), 4);

        // Final flush
        range.flush(&mut cursor).unwrap();
        assert_eq!(range.block_count(), 0);

        let data = cursor.into_inner();
        assert_eq!(&data[0..8], b"AAAABBBB");
        assert_eq!(&data[8..16], &[0u8; 8]); // blocks 2 & 3 untouched
        assert_eq!(&data[16..20], b"EEEE");
    }

    #[test]
    fn test_dirty_block_range_new_getters() {
        use std::io::Cursor;
        let mut cursor = Cursor::new(Vec::new());
        let mut range = DirtyBlockRange::new(512);

        assert_eq!(range.block_size(), 512);
        assert_eq!(range.start_block(), 0);
        assert_eq!(range.end_block(), 0);
        assert!(range.is_empty());
        assert_eq!(range.byte_len(), 0);
        assert!(range.data().is_empty());

        let block = vec![0xEE; 512];
        range.add_block(3, &block, &mut cursor).unwrap();

        assert_eq!(range.start_block(), 3);
        assert_eq!(range.end_block(), 4);
        assert_eq!(range.block_count(), 1);
        assert!(!range.is_empty());
        assert_eq!(range.byte_len(), 512);
        assert_eq!(range.data().len(), 512);

        range.add_block(4, &block, &mut cursor).unwrap();
        assert_eq!(range.start_block(), 3);
        assert_eq!(range.end_block(), 5);
        assert_eq!(range.block_count(), 2);
        assert_eq!(range.byte_len(), 1024);
        assert_eq!(range.data().len(), 1024);
    }

    #[test]
    fn test_dirty_block_range_buffer_reuse_preserves_capacity() {
        use std::io::Cursor;
        let mut cursor = Cursor::new(Vec::new());
        let mut range = DirtyBlockRange::new(1024);

        for i in 0..8 {
            range.add_block(i, &vec![0xAA; 1024], &mut cursor).unwrap();
        }
        assert_eq!(range.byte_len(), 8192);
        assert_eq!(range.block_count(), 8);

        let cap_before = range.capacity();
        assert!(cap_before >= 8192, "Initial capacity must be at least 8KB");

        range.reset();

        assert_eq!(range.byte_len(), 0);
        assert_eq!(range.block_count(), 0);
        assert_eq!(range.start_block(), 0);
        assert_eq!(range.end_block(), 0);
        assert!(range.is_empty());
        assert_eq!(
            range.capacity(),
            cap_before,
            "Buffer capacity must be preserved across reset() calls"
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

        let config = Config::test_default(src, dst);
        let engine = LocalSyncEngine::new(MockHashStore::new(), config);

        let cancel_token = std::sync::atomic::AtomicBool::new(true);
        let result = engine.run_cancellable_full_scan(&cancel_token);

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
    fn test_sync_file_directory_creation() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(source.join("new_folder")).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, config);

        // sync_file on a directory path should create the folder on dest and return Ok(())
        engine.sync_file(Path::new("new_folder")).unwrap();
        assert!(dest.join("new_folder").is_dir());
    }

    #[test]
    fn test_sync_file_truncated_dest_recovers_missing_blocks() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(4)
            .block_size_bytes(4)
            .build();
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        // 12 bytes = 3 blocks of 4 bytes
        fs::write(source.join("file.bin"), b"AAAABBBBCCCC").unwrap();
        engine.sync_file(Path::new("file.bin")).unwrap();
        assert_eq!(fs::read(dest.join("file.bin")).unwrap(), b"AAAABBBBCCCC");

        // Truncate dest file to 4 bytes simulating interrupted transfer
        let dest_file = OpenOptions::new()
            .write(true)
            .open(dest.join("file.bin"))
            .unwrap();
        dest_file.set_len(4).unwrap();
        drop(dest_file);

        // Advance source mtime to bypass mtime fast path
        let src_file = OpenOptions::new()
            .write(true)
            .open(source.join("file.bin"))
            .unwrap();
        src_file
            .set_times(
                fs::FileTimes::new()
                    .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
            )
            .unwrap();
        drop(src_file);

        // Sync again: Even though DB has matching hashes for blocks 1 and 2,
        // dest_len < expected means blocks 1 and 2 must be written and NOT zero-filled!
        engine.sync_file(Path::new("file.bin")).unwrap();
        assert_eq!(fs::read(dest.join("file.bin")).unwrap(), b"AAAABBBBCCCC");
    }

    #[test]
    fn test_sync_small_file_skips_block_hashes() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(256)
            .build();
        let store = SqliteHashStore::new(&db_path, &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        fs::write(source.join("small.txt"), b"under threshold").unwrap();
        engine.sync_file(Path::new("small.txt")).unwrap();

        assert_eq!(
            fs::read(dest.join("small.txt")).unwrap(),
            b"under threshold"
        );
        let block_hashes = engine.db.get_block_hashes(Path::new("small.txt")).unwrap();
        assert!(
            block_hashes.is_empty(),
            "Small files should not record block hashes"
        );
    }

    #[test]
    fn test_source_offline_guards_deletions() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        // Create a pre-existing file on destination
        fs::write(dest.join("keep_me.txt"), b"pre-existing").unwrap();

        let config = Config::builder(source)
            .dest_dir(dest.clone())
            .debounce_seconds(0)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .build();
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let engine = LocalSyncEngine::new(store, config.clone());
        let context = SyncWorkerContext::new(0, config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        // Queue deletion while source is offline
        tx.send(SyncCommand::FileDeleted(PathBuf::from("keep_me.txt")))
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(300));

        // Destination file must NOT be deleted while source is offline
        assert!(dest.join("keep_me.txt").exists());
    }

    #[test]
    fn test_worker_network_offline_bailout() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst_offline");
        fs::create_dir_all(&source).unwrap();
        // dest is deliberately NOT created so it is offline

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .debounce_seconds(0)
            .retry_interval_seconds(1)
            .build();
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let engine = LocalSyncEngine::new(store, config.clone());
        let context = SyncWorkerContext::new(0, config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        fs::write(source.join("file1.txt"), b"hello").unwrap();
        tx.send(SyncCommand::FileModified(PathBuf::from("file1.txt")))
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(300));
        // Dest should not exist and worker must not crash
        assert!(!dest.exists());
    }

    #[test]
    fn test_sync_file_small_file_verify_writes() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src.clone(), dst.clone());
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone());
        let engine = LocalSyncEngine::new(
            MockHashStore::new(),
            TargetSyncConfig {
                verify_writes: true,
                ..target_cfg
            },
        );
        std::fs::write(src.join("small.txt"), b"payload").unwrap();
        let mut scratch = vec![0u8; 4096];
        assert!(
            engine
                .sync_file_to_dest_buffered(Path::new("small.txt"), &dst, &mut scratch)
                .is_ok()
        );
        assert_eq!(std::fs::read(dst.join("small.txt")).unwrap(), b"payload");
    }

    #[test]
    fn test_verify_small_file_write_corruption() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src.txt");
        let dst = temp.path().join("dst.txt");
        std::fs::write(&src, b"good data").unwrap();
        std::fs::write(&dst, b"bad data").unwrap();
        match verify_small_file_write(&src, &dst) {
            Err(SyncError::WriteVerificationFailed { path, .. }) => assert_eq!(path, dst),
            other => panic!("Expected WriteVerificationFailed, got {other:?}"),
        }
        assert!(verify_small_file_write(&src, &src).is_ok());
    }

    #[test]
    fn test_sync_file_toctou_set_len_uses_total_bytes_read() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .block_sync_threshold_bytes(1024)
            .build();
        let engine = LocalSyncEngine::new(MockHashStore::new(), config);

        std::fs::write(src.join("file.bin"), vec![0xEEu8; 2048]).unwrap();
        let mut scratch = vec![0u8; 512];
        engine
            .sync_file_to_dest_buffered(Path::new("file.bin"), &dst, &mut scratch)
            .unwrap();
        assert_eq!(std::fs::metadata(dst.join("file.bin")).unwrap().len(), 2048);

        // Actual file shrank to 1024 bytes, but stale src_size of 2048 is passed
        std::fs::write(src.join("file.bin"), vec![0x55u8; 1024]).unwrap();
        let src_file = src.join("file.bin");
        let dst_file = dst.join("file.bin");
        let task = FileSyncTask {
            rel_path: Path::new("file.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 2048,
            src_mod: 12345,
            cached_id: None,
        };
        engine.sync_delta_large_file(&task, &mut scratch).unwrap();
        let dest_len = std::fs::metadata(dst.join("file.bin")).unwrap().len();
        assert_eq!(
            dest_len, 1024,
            "set_len must use total_bytes_read (1024), not stale src_size (2048)"
        );
    }

    #[test]
    fn test_sync_delta_large_file_verify_writes_success() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .verify_writes(true)
            .build();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone());
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let src_file = src.join("large.bin");
        let dst_file = dst.join("large.bin");
        std::fs::write(&src_file, vec![0xEE; 1024]).unwrap();

        let mut scratch = vec![0u8; 512];
        let task = FileSyncTask {
            rel_path: Path::new("large.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 1024,
            src_mod: 12345,
            cached_id: None,
        };
        assert!(engine.sync_delta_large_file(&task, &mut scratch).is_ok());
        assert_eq!(std::fs::read(&dst_file).unwrap(), vec![0xEE; 1024]);
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

    #[cfg(windows)]
    #[test]
    fn test_sync_refuses_dest_symlink_overwrite() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src.clone(), dst.clone());
        let engine = LocalSyncEngine::new(MockHashStore::new(), config);
        std::fs::write(src.join("target.txt"), b"content").unwrap();
        let link_target = temp.path().join("link_target");
        std::fs::create_dir_all(&link_target).unwrap();
        create_test_junction(&link_target, &dst.join("target.txt")).unwrap();

        let mut scratch = vec![0u8; 4096];
        let res = engine.sync_file_to_dest_buffered(Path::new("target.txt"), &dst, &mut scratch);
        assert!(
            res.is_err(),
            "Engine must return Err when dest is a symlink/reparse point"
        );
    }

    struct StubValidationFailEngine {
        call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl SyncEngine for StubValidationFailEngine {
        fn sync_file(&self, _path: &Path) -> Result<(), SyncError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(SyncError::validation("Permanent validation failure"))
        }
        fn sync_file_buffered(&self, _path: &Path, _scratch: &mut [u8]) -> Result<(), SyncError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(SyncError::validation("Permanent validation failure"))
        }
        fn delete_file(&self, _path: &Path) -> Result<(), SyncError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(SyncError::validation("Permanent validation failure"))
        }
        fn sync_file_to_dest_buffered(
            &self,
            _path: &Path,
            _dest_dir: &Path,
            _scratch: &mut [u8],
        ) -> Result<(), SyncError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(SyncError::validation("Permanent validation failure"))
        }
        fn delete_file_from_dest(&self, _path: &Path, _dest_dir: &Path) -> Result<(), SyncError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(SyncError::validation("Permanent validation failure"))
        }
        fn run_cancellable_full_scan(
            &self,
            _cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<ScanOutcome, SyncError> {
            Ok(ScanOutcome::Success { synced: 0 })
        }
    }

    #[test]
    fn test_sync_worker_evicts_validation_errors_without_retry() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let engine = StubValidationFailEngine {
            call_count: call_count.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src)
            .dest_dir(dst)
            .debounce_seconds(0)
            .retry_interval_seconds(1)
            .build();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, config, engine, rx, None, source_online);
        let handle = start_sync_worker(ctx).unwrap();
        tx.send(SyncCommand::FileModified(PathBuf::from(
            "unsafe/../file.txt",
        )))
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(tx);
        handle.join().unwrap();
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Validation error must be evicted after 1 attempt, not retried"
        );
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
            .build();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone());
        let engine = LocalSyncEngine::new(db, target_cfg);

        let outcome = engine.run_full_scan().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });
        assert!(!dst.join(".syncdir_archive").exists());
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
    fn test_mock_sync_engine_recording_and_errors() {
        let mock = MockSyncEngine::new();
        let mut scratch = [0u8; 64];
        let p1 = Path::new("a.txt");
        let d1 = Path::new("C:\\dest1");

        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_ok()
        );
        assert_eq!(
            mock.synced_calls(),
            vec![(p1.to_path_buf(), d1.to_path_buf())]
        );

        let p2 = Path::new("b.txt");
        let d2 = Path::new("C:\\dest2");
        assert!(mock.delete_file_from_dest(p2, d2).is_ok());
        assert_eq!(
            mock.deleted_calls(),
            vec![(p2.to_path_buf(), d2.to_path_buf())]
        );

        mock.set_sync_error(|| SyncError::validation("simulated failure"));
        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_err()
        );
        assert!(mock.delete_file_from_dest(p2, d2).is_err());

        mock.clear_sync_error();
        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_ok()
        );

        assert_eq!(mock.full_scans_count(), 0);
        let outcome = mock.run_full_scan().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 0 });
        assert_eq!(mock.full_scans_count(), 1);
    }

    #[test]
    fn test_source_connectivity_tracker() {
        let tracker = SourceConnectivityTracker::new(true);
        assert!(tracker.is_online());
        tracker.set_online(false);
        assert!(!tracker.is_online());
        let raw = tracker.raw_arc();
        assert!(!raw.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn test_debounce_queue_operations() {
        let mut queue = DebounceQueue::new(2);
        let p1 = PathBuf::from("a.txt");
        let p2 = PathBuf::from("b.txt");
        let p3 = PathBuf::from("c.txt");

        assert!(queue.enqueue_sync(p1.clone(), std::time::Duration::from_millis(10)));
        assert!(queue.enqueue_delete(p2.clone(), std::time::Duration::from_millis(20)));
        // At capacity (2 items):
        assert!(!queue.enqueue_sync(p3, std::time::Duration::from_millis(10)));
        // Existing path can be refreshed even at capacity:
        assert!(queue.enqueue_sync(p1.clone(), std::time::Duration::from_millis(50)));

        assert_eq!(queue.pending_sync_count(), 1);
        assert_eq!(queue.pending_delete_count(), 1);

        // Before deadline, draining returns empty:
        let drained = queue.drain_ready_syncs(Instant::now());
        assert!(drained.is_empty());

        // Drain after deadline:
        let future = Instant::now() + std::time::Duration::from_secs(1);
        let ready_syncs = queue.drain_ready_syncs(future);
        assert_eq!(ready_syncs, vec![p1.clone()]);
        assert_eq!(queue.pending_sync_count(), 0);

        let ready_deletes = queue.drain_ready_deletes(future);
        assert_eq!(ready_deletes, vec![p2]);
        assert_eq!(queue.pending_delete_count(), 0);

        // Requeue retry
        queue.requeue_sync_retry(p1.clone(), std::time::Duration::from_millis(50));
        assert_eq!(queue.pending_sync_count(), 1);
    }

    #[test]
    fn test_debounce_queue_min_heap_correctness() {
        let mut queue = DebounceQueue::new(10);
        let pa = PathBuf::from("a.txt");
        let pb = PathBuf::from("b.txt");
        let pc = PathBuf::from("c.txt");
        let pd = PathBuf::from("d.txt");

        let t0 = Instant::now();
        queue.enqueue_sync(pa.clone(), std::time::Duration::from_millis(50));
        queue.enqueue_sync(pb.clone(), std::time::Duration::from_millis(10));
        queue.enqueue_delete(pc.clone(), std::time::Duration::from_millis(30));

        // pb should be earliest (~10ms)
        let dl1 = queue.earliest_deadline().unwrap();
        assert!(dl1 <= t0 + std::time::Duration::from_millis(20));

        // Overwrite pb with later deadline (~100ms) -> pc should now be earliest (~30ms)
        queue.enqueue_sync(pb.clone(), std::time::Duration::from_millis(100));
        let dl2 = queue.earliest_deadline().unwrap();
        assert!(dl2 <= t0 + std::time::Duration::from_millis(40));

        // Requeue retry for pd (~5ms) -> pd should now be earliest
        queue.requeue_sync_retry(pd.clone(), std::time::Duration::from_millis(5));
        let dl3 = queue.earliest_deadline().unwrap();
        assert!(dl3 <= t0 + std::time::Duration::from_millis(15));

        // Drain up to 35ms -> pd (5ms) and pc (30ms) drained
        let drained_sync = queue.drain_ready_syncs(t0 + std::time::Duration::from_millis(35));
        assert_eq!(drained_sync, vec![pd]);
        let drained_del = queue.drain_ready_deletes(t0 + std::time::Duration::from_millis(35));
        assert_eq!(drained_del, vec![pc]);

        // pa (~50ms) should now be earliest
        let dl4 = queue.earliest_deadline().unwrap();
        assert!(dl4 <= t0 + std::time::Duration::from_millis(60));
    }

    #[test]
    fn test_debounce_queue_stress_10k_entries() {
        let mut queue = DebounceQueue::new(20_000);
        for i in 0..10_000 {
            let path = PathBuf::from(format!("dir/file_{}.txt", i));
            let delay = std::time::Duration::from_millis((i % 500 + 1) as u64);
            queue.enqueue_sync(path, delay);
        }

        let start = Instant::now();
        for _ in 0..1000 {
            let dl = queue.earliest_deadline();
            assert!(dl.is_some());
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "1000 earliest_deadline queries on 10k items took {:?}, expected < 50ms (O(1) peek)",
            elapsed
        );
    }

    #[test]
    fn test_sync_worker_state_failure_tracking() {
        let mut state = SyncWorkerState::new(1024);
        assert_eq!(state.scratch.len(), 1024);
        let p = Path::new("failed.txt");
        assert_eq!(state.record_failure(p), 1);
        assert_eq!(state.record_failure(p), 2);
        state.reset_failure(p);
        assert_eq!(state.record_failure(p), 1);
    }

    #[test]
    fn test_reachability_monitor() {
        let temp = tempdir().unwrap();
        let target = temp.path().to_path_buf();
        let mock_resolver = std::sync::Arc::new(crate::net::MockNetworkResolver::new());
        let mut monitor = ReachabilityMonitor::new(0, target.clone(), 5, mock_resolver);

        assert_eq!(monitor.active_dest(), target.as_path());
        assert!(!monitor.is_dest_online());

        let now = Instant::now();
        monitor.check_reachability(now, None);
        assert!(monitor.is_dest_online());
    }

    #[test]
    fn test_calculate_exponential_backoff() {
        let base = std::time::Duration::from_secs(5);
        assert_eq!(
            calculate_exponential_backoff(1, base),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            calculate_exponential_backoff(2, base),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            calculate_exponential_backoff(3, base),
            std::time::Duration::from_secs(20)
        );
        assert_eq!(
            calculate_exponential_backoff(4, base),
            std::time::Duration::from_secs(40)
        );
        assert_eq!(
            calculate_exponential_backoff(10, base),
            std::time::Duration::from_secs(300)
        );
    }

    #[test]
    fn test_write_verification_retained_in_pending_syncs() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .debounce_seconds(0)
            .retry_interval_seconds(0)
            .build();

        let engine = MockSyncEngine::new();
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        engine.set_sync_handler(move |path| {
            let count = call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Err(SyncError::write_verification_failed(path.to_path_buf()))
            } else {
                Ok(())
            }
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, config, engine.clone(), rx, None, source_online);
        let handle = start_sync_worker(ctx).unwrap();

        tx.send(SyncCommand::FileModified(PathBuf::from("data.txt")))
            .unwrap();

        let start = Instant::now();
        while engine.synced_calls().is_empty()
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        drop(tx);
        handle.join().unwrap();

        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "WriteVerificationFailed must be retried and not evicted after first failure"
        );
        assert_eq!(engine.synced_calls().len(), 1);
        assert_eq!(engine.synced_calls()[0].0, PathBuf::from("data.txt"));
    }

    #[test]
    fn test_queue_overflow_triggers_catchup_scan() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(0)
            .retry_interval_seconds(0)
            .build();

        let engine = MockSyncEngine::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, config, engine.clone(), rx, None, source_online)
            .with_max_pending_queue(5);
        let handle = start_sync_worker(ctx).unwrap();

        // Wait for initial catch-up full scan on destination startup
        let start = Instant::now();
        while engine.full_scans_count() == 0 && start.elapsed() < std::time::Duration::from_secs(2)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let base_scans = engine.full_scans_count();
        assert!(
            base_scans >= 1,
            "Initial scan on destination reconnect should complete"
        );

        // Enqueue 10 distinct file paths to trigger queue overflow (capacity is 5)
        for i in 0..10 {
            let _ = tx.send(SyncCommand::FileModified(PathBuf::from(format!(
                "file_{}.txt",
                i
            ))));
        }

        // Wait for queue to drain and catchup scan to trigger
        let start = Instant::now();
        while engine.full_scans_count() <= base_scans
            && start.elapsed() < std::time::Duration::from_secs(2)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        drop(tx);
        handle.join().unwrap();

        assert!(
            engine.full_scans_count() > base_scans,
            "Queue overflow must trigger an additional catchup full scan after queue drains"
        );
    }

    #[test]
    fn test_worker_sync_and_delete_uses_resolved_unc_path() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let fake_dest = PathBuf::from(r"Z:\mapped_share");
        let real_unc_dest = dir.path().join("unc_share");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&real_unc_dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(fake_dest.clone())
            .debounce_seconds(0)
            .retry_interval_seconds(1)
            .build();

        let resolver = std::sync::Arc::new(crate::net::MockNetworkResolver::new());
        resolver.set_alternate_path(fake_dest, real_unc_dest.clone());

        let engine = MockSyncEngine::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let ctx = SyncWorkerContext::new(0, config, engine.clone(), rx, None, source_online)
            .with_resolver(resolver);
        let handle = start_sync_worker(ctx).unwrap();

        tx.send(SyncCommand::FileModified(PathBuf::from("doc.txt")))
            .unwrap();
        tx.send(SyncCommand::FileDeleted(PathBuf::from("old.txt")))
            .unwrap();

        let start = Instant::now();
        while (engine.synced_calls().is_empty() || engine.deleted_calls().is_empty())
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        drop(tx);
        handle.join().unwrap();

        assert_eq!(engine.synced_calls().len(), 1);
        assert_eq!(engine.synced_calls()[0].0, PathBuf::from("doc.txt"));
        assert_eq!(engine.synced_calls()[0].1, real_unc_dest);

        assert_eq!(engine.deleted_calls().len(), 1);
        assert_eq!(engine.deleted_calls()[0].0, PathBuf::from("old.txt"));
        assert_eq!(engine.deleted_calls()[0].1, real_unc_dest);
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
        // Insert record with forward slash key as SQLite would store it
        let rec = FileRecord::new(
            PathBuf::from("nested/file.txt"),
            7,
            safe_modified_millis(&fs::metadata(&test_file).unwrap()).unwrap(),
        )
        .with_id(1);
        store.save_file(&rec, &[]).unwrap();

        let engine = LocalSyncEngine::new(store, config);
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
        // Depth 65 triggers max depth skip which sets scan_complete = false
        scan_dir(&root, &root, &mut files, &mut scan_complete, 65).unwrap();
        assert!(!scan_complete);
    }

    struct FailingHashStore;
    impl HashStore for FailingHashStore {
        fn get_file(&self, _path: &Path) -> Result<Option<FileRecord>, SyncError> {
            Ok(None)
        }
        fn save_file(&self, _record: &FileRecord, _hashes: &[BlockHash]) -> Result<(), SyncError> {
            Ok(())
        }
        fn get_block_hashes(&self, _path: &Path) -> Result<Vec<BlockHash>, SyncError> {
            Ok(vec![])
        }
        fn delete_file(&self, _path: &Path) -> Result<(), SyncError> {
            Ok(())
        }
        fn list_files(&self) -> Result<Vec<PathBuf>, SyncError> {
            Ok(vec![])
        }
        fn list_all_records(&self) -> Result<HashMap<PathBuf, FileRecord>, SyncError> {
            Err(SyncError::db("Forced list_all_records failure"))
        }
        fn save_files_batch(
            &self,
            _records: &[(&FileRecord, &[BlockHash])],
        ) -> Result<(), SyncError> {
            Ok(())
        }
    }

    #[test]
    fn test_full_scan_db_error_propagated() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src, dst);
        let engine = LocalSyncEngine::new(FailingHashStore, config);
        let result = engine.run_full_scan();
        assert!(matches!(result, Err(SyncError::Db(..))));
    }

    #[test]
    fn test_worker_io_backoff() {
        let base = std::time::Duration::from_secs(2);
        let b1 = calculate_exponential_backoff(1, base);
        let b2 = calculate_exponential_backoff(2, base);
        let b3 = calculate_exponential_backoff(3, base);
        assert!(b1 <= b2);
        assert!(b2 <= b3);
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
        let engine =
            LocalSyncEngine::new(MockHashStore::new(), config).with_resolved_dest(&alt_dst);
        assert_eq!(engine.resolved_dest(), Some(alt_dst.as_path()));

        let outcome = engine.run_full_scan().unwrap();
        assert!(matches!(outcome, ScanOutcome::Success { synced: 1 }));
        assert!(alt_dst.join("hello.txt").exists());
    }
}

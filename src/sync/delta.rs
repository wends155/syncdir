use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::num::NonZeroU64;
use std::time::SystemTime;

use crate::config::{TargetSyncConfig, VerificationMode};
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;

use super::engine::{
    DirtyRangeLease, FileSyncTask, LocalSyncEngine, safe_epoch_duration_millis,
    safe_modified_millis,
};

/// Contiguous range of dirty blocks to coalesce delta writes and reduce seek overhead.
#[derive(Debug)]
pub struct DirtyBlockRange {
    start_block: u64,
    block_count: u64,
    block_size: NonZeroU64,
    data: Vec<u8>,
}

impl DirtyBlockRange {
    /// Maximum coalesced batch size in bytes (16MB).
    pub const MAX_COALESCE_BYTES: usize = 16 * 1024 * 1024;

    /// Create an empty dirty block range with pinned block size.
    ///
    /// # Arguments
    /// Create an empty dirty block range from a validated non-zero block size.
    ///
    /// # Arguments
    ///
    /// * `block_size` - A non-zero block size in bytes.
    ///
    /// # Returns
    ///
    /// An empty [`DirtyBlockRange`].
    pub fn new(block_size: NonZeroU64) -> Self {
        Self {
            start_block: 0,
            block_count: 0,
            block_size,
            data: Vec::new(),
        }
    }

    /// Create an empty dirty block range from a validated non-zero block size.
    ///
    /// Alias for [`new`](Self::new).
    #[inline]
    pub fn new_nonzero(block_size: NonZeroU64) -> Self {
        Self::new(block_size)
    }

    /// Create an empty dirty block range, returning `SyncError::Validation` if `block_size` is 0.
    ///
    /// # Arguments
    ///
    /// * `block_size` - Block size in bytes.
    ///
    /// # Returns
    ///
    /// `Ok(DirtyBlockRange)` if `block_size > 0`.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Validation`] if `block_size` is 0.
    pub fn try_new(block_size: u64) -> Result<Self, SyncError> {
        let non_zero = NonZeroU64::new(block_size).ok_or_else(|| {
            SyncError::validation("DirtyBlockRange block_size must be greater than zero")
        })?;
        Ok(Self::new(non_zero))
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
        self.block_size.get()
    }

    /// Return the pinned block size as a `NonZeroU64`.
    #[must_use]
    pub fn block_size_nonzero(&self) -> NonZeroU64 {
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
    pub fn add_block<W: Write + Seek>(
        &mut self,
        block_idx: u64,
        block_bytes: &[u8],
        writer: &mut W,
    ) -> Result<(), SyncError> {
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
    pub fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> Result<(), SyncError> {
        if self.block_count > 0 {
            let offset = self.start_block * self.block_size.get();
            if let Err(e) = writer.seek(SeekFrom::Start(offset)) {
                self.reset();
                return Err(SyncError::Io(e));
            }
            if let Err(e) = writer.write_all(&self.data) {
                self.reset();
                return Err(SyncError::Io(e));
            }
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

impl TryFrom<u64> for DirtyBlockRange {
    type Error = SyncError;

    fn try_from(val: u64) -> Result<Self, Self::Error> {
        Self::try_new(val)
    }
}

impl Default for DirtyBlockRange {
    fn default() -> Self {
        const DEFAULT_BLOCK_SIZE: NonZeroU64 = match NonZeroU64::new(64 * 1024) {
            Some(v) => v,
            None => unreachable!(),
        };
        Self::new(DEFAULT_BLOCK_SIZE)
    }
}

/// Read exactly `buf.len()` bytes or until EOF, handling partial reads.
pub(crate) fn read_block<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, std::io::Error> {
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

/// Collaborating transfer engine for large-file chunk hashing and in-place delta updates.
pub(crate) struct DeltaTransferEngine<S: HashStore> {
    db: S,
    config: TargetSyncConfig,
    dirty_range_pool: std::sync::Mutex<Option<DirtyBlockRange>>,
}

impl<S: HashStore> DeltaTransferEngine<S> {
    pub(crate) fn new(db: S, config: TargetSyncConfig) -> Self {
        Self {
            db,
            config,
            dirty_range_pool: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn acquire_dirty_range_lease(&self) -> DirtyRangeLease<'_> {
        let mut pool = self
            .dirty_range_pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let range = pool
            .take()
            .unwrap_or_else(|| DirtyBlockRange::new(self.config.block_size_nonzero()));
        DirtyRangeLease::new(&self.dirty_range_pool, range)
    }

    #[cfg(test)]
    pub(crate) fn dirty_range_capacity(&self) -> usize {
        self.dirty_range_pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|r| r.capacity())
            .unwrap_or(0)
    }

    pub(crate) fn sync_delta_large_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        let mut dirty_blocks_written: usize = 0;
        let run =
            || -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
                if let Some(parent) = task.dest_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut src_file = File::open(task.src_path)?;
                let mut dest_file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(task.dest_path)?;

                let block_size = self.config.block_size_bytes();
                let initial_dest_len = dest_file.metadata().map(|m| m.len()).unwrap_or(0);
                let cached_hashes = self.db.get_block_hashes(task.rel_path)?;

                let buffer = scratch;
                let mut new_hashes = Vec::new();
                let mut block_idx: u64 = 0;
                let mut lease = self.acquire_dirty_range_lease();
                let range = &mut *lease;
                let mut total_bytes_read: u64 = 0;
                let mut modified_block_indices: Vec<(u64, usize, [u8; 32])> = Vec::new();

                loop {
                    let bytes_read = read_block(&mut src_file, buffer)?;
                    if bytes_read == 0 {
                        break;
                    }
                    total_bytes_read += bytes_read as u64;
                    let hash = *blake3::hash(&buffer[..bytes_read]).as_bytes();
                    new_hashes.push(hash);

                    let cached = cached_hashes.get(block_idx as usize);
                    let block_changed = match cached {
                        Some(cached_hash) => cached_hash != &hash,
                        None => true,
                    };
                    let is_truncated_on_dest =
                        initial_dest_len < (block_idx * block_size + bytes_read as u64);

                    if block_changed || is_truncated_on_dest {
                        range.add_block(block_idx, &buffer[..bytes_read], &mut dest_file)?;
                        dirty_blocks_written += 1;
                        if self.config.verification_mode() != VerificationMode::Disabled {
                            modified_block_indices.push((block_idx, bytes_read, hash));
                        }
                    }
                    block_idx += 1;
                }
                range.flush(&mut dest_file)?;
                drop(lease);

                // Post-stream metadata re-verification (TOCTOU protection)
                let post_meta = src_file.metadata()?;
                let post_len = post_meta.len();
                let post_mod = safe_modified_millis(&post_meta)?;
                if post_len != total_bytes_read || post_mod != task.src_mod {
                    tracing::warn!(
                        path = %task.rel_path.display(),
                        expected_len = total_bytes_read,
                        post_len,
                        expected_mod = task.src_mod,
                        post_mod,
                        "Source file modified concurrently during delta streaming; aborting sync"
                    );
                    return Err(SyncError::write_verification_failed(
                        task.dest_path.to_path_buf(),
                    ));
                }

                // Validate destination length before verification (TOCTOU / remote corruption protection)
                let dest_len_pre = dest_file.metadata()?.len();
                if dest_len_pre < total_bytes_read {
                    tracing::warn!(
                        path = %task.rel_path.display(),
                        dest_len_pre,
                        total_bytes_read,
                        "Destination file is shorter than expected bytes read before truncation"
                    );
                    return Err(SyncError::write_verification_failed(
                        task.dest_path.to_path_buf(),
                    ));
                }

                let verification_mode = self.config.verification_mode();
                let indices_to_verify: Vec<&(u64, usize, [u8; 32])> = match verification_mode {
                    VerificationMode::Disabled => Vec::new(),
                    VerificationMode::MetadataAndFlush => {
                        dest_file.sync_all()?;
                        Vec::new()
                    }
                    VerificationMode::Full => {
                        dest_file.sync_all()?;
                        modified_block_indices.iter().collect()
                    }
                    VerificationMode::Sampled => {
                        dest_file.sync_all()?;
                        let n = modified_block_indices.len();
                        if n <= 4 {
                            modified_block_indices.iter().collect()
                        } else {
                            let mut sample = Vec::with_capacity(4);
                            sample.push(&modified_block_indices[0]);
                            let mid1 = n / 3;
                            let mid2 = (2 * n) / 3;
                            sample.push(&modified_block_indices[mid1]);
                            sample.push(&modified_block_indices[mid2]);
                            sample.push(&modified_block_indices[n - 1]);
                            sample
                        }
                    }
                };

                let mut current_offset: Option<u64> = None;
                for &(b_idx, bytes_len, expected_hash) in indices_to_verify {
                    let target_offset = b_idx * block_size;
                    if current_offset != Some(target_offset) {
                        dest_file.seek(SeekFrom::Start(target_offset))?;
                    }
                    if let Err(e) = dest_file.read_exact(&mut buffer[..bytes_len]) {
                        if e.kind() == std::io::ErrorKind::UnexpectedEof {
                            return Err(SyncError::write_verification_failed_block(
                                task.dest_path.to_path_buf(),
                                b_idx,
                                expected_hash,
                                [0u8; 32],
                            ));
                        }
                        return Err(SyncError::Io(e));
                    }
                    current_offset = Some(target_offset + bytes_len as u64);
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

                // Truncate to exact bytes read (TOCTOU protection)
                dest_file.set_len(total_bytes_read)?;

                let dest_len_final = dest_file.metadata()?.len();
                if dest_len_final != total_bytes_read {
                    tracing::warn!(
                        path = %task.rel_path.display(),
                        dest_len_final,
                        total_bytes_read,
                        "Destination file length does not match total bytes read after set_len"
                    );
                    return Err(SyncError::write_verification_failed(
                        task.dest_path.to_path_buf(),
                    ));
                }

                if let Err(e) = dest_file.set_times(fs::FileTimes::new().set_modified(
                    SystemTime::UNIX_EPOCH + safe_epoch_duration_millis(task.src_mod),
                )) {
                    tracing::warn!(
                        path = %task.rel_path.display(),
                        error = %e,
                        "Failed to preserve modified timestamp on destination file (delta)"
                    );
                }

                let record = FileRecord {
                    id: task.cached_id,
                    relative_path: task.rel_path.to_path_buf(),
                    file_size: total_bytes_read as i64,
                    last_modified: task.src_mod,
                };
                tracing::info!(
                    path = %task.rel_path.display(),
                    target = %task.dest_dir.display(),
                    size = total_bytes_read,
                    "Synced file to destination (delta)"
                );
                Ok((record, new_hashes))
            };

        match run() {
            Ok(result) => Ok(result),
            Err(e) => {
                if dirty_blocks_written > 0 {
                    tracing::warn!(
                        path = %task.rel_path.display(),
                        error = %e,
                        "Delta sync failed after writing dirty blocks; invalidating SQLite cache to prevent false hits"
                    );
                    let _ = self.db.delete_file(task.rel_path);
                }
                Err(e)
            }
        }
    }
}

impl<S: HashStore> LocalSyncEngine<S> {
    #[cfg(test)]
    pub(crate) fn sync_delta_large_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        self.delta_engine.sync_delta_large_file_core(task, scratch)
    }

    #[cfg(test)]
    pub(crate) fn sync_delta_large_file(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let (record, hashes) = self.sync_delta_large_file_core(task, scratch)?;
        self.db.save_file(&record, &hashes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use crate::db::{MockHashStore, SqliteHashStore};
    use crate::sync::SyncEngine;
    use std::path::{Path, PathBuf};
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

    #[test]
    fn test_read_block_fills_complete_buffer() {
        let data = vec![0xABu8; 1024];
        let mut reader = std::io::Cursor::new(&data);
        let mut buf = vec![0u8; 1024];
        let n = read_block(&mut reader, &mut buf).unwrap();
        assert_eq!(n, 1024);
        assert_eq!(buf, data);
    }

    #[test]
    fn test_dirty_block_range_coalescing() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut range = DirtyBlockRange::new(NonZeroU64::new(4).unwrap());

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
        assert_eq!(&data[16..20], b"EEEE");
    }

    #[test]
    fn test_dirty_block_range_new_getters() {
        let range = DirtyBlockRange::new(NonZeroU64::new(1024).unwrap());
        assert_eq!(range.block_size(), 1024);
        assert_eq!(range.start_block(), 0);
        assert_eq!(range.end_block(), 0);
        assert_eq!(range.block_count(), 0);
        assert!(range.is_empty());
        assert_eq!(range.byte_len(), 0);
        assert_eq!(range.data(), &[] as &[u8]);
    }

    #[test]
    fn test_dirty_block_range_rejects_zero() {
        assert!(matches!(
            DirtyBlockRange::try_new(0),
            Err(SyncError::Validation { ref message, .. }) if message.contains("greater than zero")
        ));
        assert!(DirtyBlockRange::try_new(65536).is_ok());

        assert!(matches!(
            DirtyBlockRange::try_from(0),
            Err(SyncError::Validation { .. })
        ));
        assert!(DirtyBlockRange::try_from(65536).is_ok());
    }

    #[test]
    fn test_dirty_block_range_try_new_rejects_zero() {
        assert!(DirtyBlockRange::try_new(0).is_err());
    }

    #[test]
    fn test_dirty_block_range_buffer_reuse_preserves_capacity() {
        let mut range = DirtyBlockRange::new(NonZeroU64::new(4).unwrap());
        let mut cursor = std::io::Cursor::new(Vec::new());

        range.add_block(0, b"AAAA", &mut cursor).unwrap();
        range.add_block(1, b"BBBB", &mut cursor).unwrap();
        assert_eq!(range.byte_len(), 8);
        let cap_before = range.capacity();
        assert!(cap_before >= 8);

        range.reset();
        assert_eq!(range.block_count(), 0);
        assert_eq!(range.start_block(), 0);
        assert_eq!(range.byte_len(), 0);
        assert!(range.is_empty());
        assert_eq!(
            range.capacity(),
            cap_before,
            "reset() must preserve allocated buffer capacity"
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
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // 8 bytes payload = exactly 2 blocks of 4 bytes
        fs::write(source.join("exact.bin"), b"12345678").unwrap();
        engine.sync_file(Path::new("exact.bin")).unwrap();

        assert_eq!(fs::read(dest.join("exact.bin")).unwrap(), b"12345678");
        let hashes = engine.db.get_block_hashes(Path::new("exact.bin")).unwrap();
        assert_eq!(hashes.len(), 2);
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
        let store = SqliteHashStore::new(
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

        // 12 bytes > 10 byte threshold -> delta sync path (3 blocks of 4)
        fs::write(source.join("big.bin"), b"AAAABBBBcccc").unwrap();
        engine.sync_file(Path::new("big.bin")).unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAABBBBcccc");

        // Modify only block 1 (bytes 4-7)
        let big_bin_path = source.join("big.bin");
        fs::write(&big_bin_path, b"AAAAZZZZCCCC").unwrap();
        let f = OpenOptions::new().write(true).open(&big_bin_path).unwrap();
        f.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
        )
        .unwrap();

        engine.sync_file(Path::new("big.bin")).unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAAZZZZCCCC");

        let hashes = engine.db.get_block_hashes(Path::new("big.bin")).unwrap();
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn test_sync_delta_large_file_post_stream_metadata_reverification() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let src_file = src.join("large.bin");
        let dst_file = dst.join("large.bin");
        std::fs::write(&src_file, vec![0xAA; 1024]).unwrap();

        let mut scratch = vec![0u8; 512];
        let task = FileSyncTask {
            rel_path: Path::new("large.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 1024,
            src_mod: 999_999,
            cached_id: None,
        };
        let result = engine.sync_delta_large_file(&task, &mut scratch);
        assert!(
            matches!(result, Err(SyncError::WriteVerificationFailed { .. })),
            "Expected WriteVerificationFailed on post-stream metadata mismatch, got {result:?}"
        );
    }

    #[test]
    fn test_sync_delta_large_file_dest_len_mismatch_detected() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let src_file = src.join("large.bin");
        let dst_file = dst.join("large.bin");

        let store = MockHashStore::new();
        let h0 = *blake3::hash(&[0xEE; 512]).as_bytes();
        let h1 = *blake3::hash(&[0xEE; 512]).as_bytes();
        let rec = FileRecord {
            id: None,
            relative_path: PathBuf::from("large.bin"),
            file_size: 1024,
            last_modified: 100,
        };
        store.save_file(&rec, &[h0, h1]).unwrap();

        let dst_file_clone = dst_file.clone();
        store.set_error_hook(Some(Box::new(move |op| {
            if op == "get_block_hashes" {
                let f = OpenOptions::new()
                    .write(true)
                    .open(&dst_file_clone)
                    .unwrap();
                f.set_len(512).unwrap();
            }
            None
        })));
        let engine = LocalSyncEngine::new(store, target_cfg);

        // Pre-create destination with 1024 bytes matching source
        std::fs::write(&src_file, vec![0xEE; 1024]).unwrap();
        std::fs::write(&dst_file, vec![0xEE; 1024]).unwrap();

        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file).unwrap()).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("large.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 1024,
            src_mod,
            cached_id: Some(1),
        };

        let mut scratch = vec![0u8; 512];
        let result = engine.sync_delta_large_file(&task, &mut scratch);
        assert!(
            matches!(result, Err(SyncError::WriteVerificationFailed { .. })),
            "Expected WriteVerificationFailed when destination length is corrupted/truncated, got {result:?}"
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
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let src_file = src.join("large.bin");
        let dst_file = dst.join("large.bin");
        std::fs::write(&src_file, vec![0xCC; 1024]).unwrap();

        let mut scratch = vec![0u8; 512];
        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file).unwrap()).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("large.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 1024,
            src_mod,
            cached_id: None,
        };
        let result = engine.sync_delta_large_file(&task, &mut scratch);
        assert!(result.is_ok());
        assert_eq!(std::fs::read(&dst_file).unwrap(), vec![0xCC; 1024]);
    }

    #[test]
    fn test_local_sync_engine_dirty_range_buffer_reuse() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        // Sync file 1
        let f1 = "file1.bin";
        std::fs::write(src.join(f1), vec![0x11; 1024]).unwrap();
        let src_meta1 = std::fs::metadata(src.join(f1)).unwrap();
        let task1 = FileSyncTask {
            rel_path: Path::new(f1),
            src_path: &src.join(f1),
            dest_path: &dst.join(f1),
            dest_dir: &dst,
            src_size: src_meta1.len() as i64,
            src_mod: safe_modified_millis(&src_meta1).unwrap(),
            cached_id: None,
        };
        let mut scratch = vec![0u8; 512];
        engine.sync_delta_large_file(&task1, &mut scratch).unwrap();

        let cap1 = engine.dirty_range_capacity();
        assert!(
            cap1 >= 512,
            "dirty_range buffer must have allocated capacity"
        );

        // Sync file 2
        let f2 = "file2.bin";
        std::fs::write(src.join(f2), vec![0x22; 1024]).unwrap();
        let src_meta2 = std::fs::metadata(src.join(f2)).unwrap();
        let task2 = FileSyncTask {
            rel_path: Path::new(f2),
            src_path: &src.join(f2),
            dest_path: &dst.join(f2),
            dest_dir: &dst,
            src_size: src_meta2.len() as i64,
            src_mod: safe_modified_millis(&src_meta2).unwrap(),
            cached_id: None,
        };
        engine.sync_delta_large_file(&task2, &mut scratch).unwrap();

        let cap2 = engine.dirty_range_capacity();
        assert!(
            cap2 >= cap1,
            "dirty_range capacity should be retained or grown across files"
        );
    }

    #[test]
    fn test_dirty_block_range_reset_on_flush_error() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "simulated write failure",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Seek for FailingWriter {
            fn seek(&mut self, _pos: SeekFrom) -> std::io::Result<u64> {
                Ok(0)
            }
        }

        let mut range = DirtyBlockRange::new(NonZeroU64::new(512).unwrap());
        let mut cursor = std::io::Cursor::new(Vec::new());
        range.add_block(0, &[0xAA; 512], &mut cursor).unwrap();
        assert_eq!(range.block_count(), 1);
        assert_eq!(range.byte_len(), 512);

        let mut failing = FailingWriter;
        let res = range.flush(&mut failing);
        assert!(res.is_err());
        assert!(
            range.is_empty(),
            "DirtyBlockRange must be empty after flush error"
        );
        assert_eq!(range.block_count(), 0);
        assert_eq!(range.byte_len(), 0);
    }

    #[test]
    fn test_delta_transfer_engine_standalone() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("test.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let cfg = test_config(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::from_config(&cfg, dest.clone()).unwrap();
        let store_cfg = crate::db::StoreConfig::new(
            target_cfg.block_size_bytes(),
            target_cfg.block_sync_threshold_bytes(),
        )
        .unwrap();
        let db = SqliteHashStore::new(&db_path, store_cfg).unwrap();
        let engine = DeltaTransferEngine::new(db, target_cfg);

        let src_file = source.join("large.bin");
        let dst_file = dest.join("large.bin");
        let payload = vec![0xABu8; 1024];
        fs::write(&src_file, &payload).unwrap();

        let meta = fs::metadata(&src_file).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("large.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dest,
            src_size: meta.len() as i64,
            src_mod: safe_modified_millis(&meta).unwrap(),
            cached_id: None,
        };

        let mut scratch = vec![0u8; 512];
        let (record, block_hashes) = engine
            .sync_delta_large_file_core(&task, &mut scratch)
            .unwrap();
        assert_eq!(record.relative_path, Path::new("large.bin"));
        assert_eq!(record.file_size, 1024);
        assert_eq!(block_hashes.len(), 2);
        assert_eq!(fs::read(&dst_file).unwrap(), payload);
        assert!(
            engine.dirty_range_capacity() >= 512,
            "dirty_range_capacity should retain allocated buffer"
        );
    }
}

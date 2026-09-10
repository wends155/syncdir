use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::config::VerificationMode;
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;

use super::engine::{
    FileSyncTask, LocalSyncEngine, safe_epoch_duration_millis, safe_modified_millis,
};

/// RAII guard for temporary staging files during atomic small-file sync.
/// Automatically removes the temporary file on drop unless disarmed.
#[derive(Debug)]
pub(crate) struct TempFileGuard {
    path: PathBuf,
    disarmed: bool,
}

impl TempFileGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            disarmed: false,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Global atomic counter for unique temporary staging file generation across threads.
static TEMP_FILE_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl<S: HashStore> LocalSyncEngine<S> {
    pub(crate) fn sync_small_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        if let Some(parent) = task.dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut nonce_hasher = blake3::Hasher::new();
        nonce_hasher.update(&std::process::id().to_le_bytes());
        nonce_hasher.update(
            &SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes(),
        );
        nonce_hasher.update(
            &TEMP_FILE_NONCE
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .to_le_bytes(),
        );
        let random_nonce = u64::from_le_bytes(
            nonce_hasher.finalize().as_bytes()[..8]
                .try_into()
                .unwrap_or_default(),
        );

        let file_stem = task
            .dest_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let temp_name = format!("{file_stem}.{random_nonce:016x}.syncdir_tmp");
        let temp_path = match task.dest_path.parent() {
            Some(parent) => parent.join(temp_name),
            None => PathBuf::from(temp_name),
        };
        let mut temp_guard = TempFileGuard::new(temp_path.clone());

        let mut src_file = File::open(task.src_path)?;
        let mut temp_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temp_path)?;

        let mut hasher = blake3::Hasher::new();
        let mut total_bytes_copied: u64 = 0;
        let mut stack_chunk = [0u8; 64 * 1024];
        let buf: &mut [u8] = if scratch.len() >= 64 * 1024 {
            &mut scratch[..64 * 1024]
        } else {
            &mut stack_chunk[..]
        };

        loop {
            let n = match src_file.read(buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(SyncError::Io(e)),
            };
            hasher.update(&buf[..n]);
            temp_file.write_all(&buf[..n])?;
            total_bytes_copied += n as u64;
        }

        // Post-stream metadata re-verification (TOCTOU protection)
        let post_meta = src_file.metadata()?;
        let post_len = post_meta.len();
        let post_mod = safe_modified_millis(&post_meta)?;
        if post_len != total_bytes_copied || post_mod != task.src_mod {
            tracing::warn!(
                path = %task.rel_path.display(),
                expected_len = total_bytes_copied,
                post_len,
                expected_mod = task.src_mod,
                post_mod,
                "Source small file modified concurrently during streaming; aborting sync"
            );
            return Err(SyncError::write_verification_failed(
                task.dest_path.to_path_buf(),
            ));
        }

        // Flush and verify write based on VerificationMode
        match self.config.verification_mode() {
            VerificationMode::Disabled => {}
            VerificationMode::MetadataAndFlush => {
                let temp_meta = temp_file.metadata()?;
                if temp_meta.len() != total_bytes_copied {
                    return Err(SyncError::write_verification_failed(
                        task.dest_path.to_path_buf(),
                    ));
                }
                temp_file.sync_all()?;
            }
            VerificationMode::Sampled | VerificationMode::Full => {
                temp_file.sync_all()?;
                temp_file.seek(SeekFrom::Start(0))?;
                let mut written_hasher = blake3::Hasher::new();
                let mut verify_buf = [0u8; 64 * 1024];
                loop {
                    match temp_file.read(&mut verify_buf) {
                        Ok(0) => break,
                        Ok(n) => written_hasher.update(&verify_buf[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(SyncError::Io(e)),
                    };
                }
                if written_hasher.finalize() != hasher.finalize() {
                    return Err(SyncError::write_verification_failed(
                        task.dest_path.to_path_buf(),
                    ));
                }
            }
        }

        if let Err(e) = temp_file.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + safe_epoch_duration_millis(task.src_mod)),
        ) {
            tracing::warn!(
                path = %task.rel_path.display(),
                error = %e,
                "Failed to preserve modified timestamp on staging file; proceeding with rename"
            );
        }

        drop(temp_file);
        fs::rename(&temp_path, task.dest_path)?;
        temp_guard.disarm();

        let record = FileRecord {
            id: task.cached_id,
            relative_path: task.rel_path.to_path_buf(),
            file_size: total_bytes_copied as i64,
            last_modified: task.src_mod,
        };
        tracing::info!(
            path = %task.rel_path.display(),
            target = %task.dest_dir.display(),
            size = total_bytes_copied,
            "Synced file to destination via atomic staging"
        );
        Ok((record, Vec::new()))
    }

    #[cfg(test)]
    pub(crate) fn sync_small_file(&self, task: &FileSyncTask<'_>) -> Result<(), SyncError> {
        let mut stack_scratch = [0u8; 64 * 1024];
        let (record, _) = self.sync_small_file_core(task, &mut stack_scratch)?;
        self.db.save_file(&record, &[])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use crate::db::{MockHashStore, SqliteHashStore};
    use crate::sync::SyncEngine;
    use std::path::Path;
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
    fn test_small_file_sync() {
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

        // Write a small file (< 10 bytes threshold)
        fs::write(source.join("tiny.txt"), b"hi").unwrap();
        engine.sync_file(Path::new("tiny.txt")).unwrap();

        let content = fs::read_to_string(dest.join("tiny.txt")).unwrap();
        assert_eq!(content, "hi");

        // DB should have a record
        assert!(engine.db.get_file(Path::new("tiny.txt")).unwrap().is_some());
    }

    #[test]
    fn test_zero_byte_file_sync() {
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

        // 0-byte file
        fs::write(source.join("empty.txt"), b"").unwrap();
        engine.sync_file(Path::new("empty.txt")).unwrap();

        assert!(dest.join("empty.txt").exists());
        assert_eq!(fs::read(dest.join("empty.txt")).unwrap().len(), 0);
        assert!(
            engine
                .db
                .get_file(Path::new("empty.txt"))
                .unwrap()
                .is_some()
        );
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
            .build()
            .unwrap();
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
    fn test_sync_small_file_records_actual_bytes_copied() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .verify_writes(false)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone());
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        let src_file = src.join("small.txt");
        let dst_file = dst.join("small.txt");
        std::fs::write(&src_file, vec![0x42; 500]).unwrap();

        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file).unwrap()).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("small.txt"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 1000,
            src_mod,
            cached_id: None,
        };

        engine.sync_small_file(&task).unwrap();
        let record = store.get_file(Path::new("small.txt")).unwrap().unwrap();
        assert_eq!(
            record.file_size, 500,
            "Saved record must use actual bytes copied (500), not stale task.src_size (1000)"
        );
    }

    #[test]
    fn test_verification_mode_metadata_and_flush() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let file_name = "large_2block.bin";
        let content = vec![0xABu8; 1024];
        std::fs::write(src.join(file_name), &content).unwrap();

        let target_cfg = TargetSyncConfig::builder(src.clone(), dst.clone())
            .block_size_bytes(512)
            .block_sync_threshold_bytes(512)
            .verification_mode(VerificationMode::MetadataAndFlush)
            .build()
            .unwrap();

        assert_eq!(
            target_cfg.verification_mode(),
            VerificationMode::MetadataAndFlush
        );

        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        let src_file_path = src.join(file_name);
        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file_path).unwrap()).unwrap();

        let mut scratch = vec![0u8; 512];
        let task = FileSyncTask {
            rel_path: Path::new(file_name),
            src_path: &src_file_path,
            dest_path: &dst.join(file_name),
            dest_dir: &dst,
            src_size: 1024,
            src_mod,
            cached_id: None,
        };

        engine.sync_delta_large_file(&task, &mut scratch).unwrap();

        let dst_file = dst.join(file_name);
        assert!(dst_file.exists());
        assert_eq!(std::fs::metadata(&dst_file).unwrap().len(), 1024);
    }

    #[test]
    fn test_verification_mode_sampled() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let file_name = "large_6block.bin";
        let content = vec![0xCDu8; 1536];
        let src_file_path = src.join(file_name);
        std::fs::write(&src_file_path, &content).unwrap();
        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file_path).unwrap()).unwrap();

        let target_cfg = TargetSyncConfig::builder(src.clone(), dst.clone())
            .block_size_bytes(256)
            .block_sync_threshold_bytes(256)
            .verification_mode(VerificationMode::Sampled)
            .build()
            .unwrap();

        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        let mut scratch = vec![0u8; 256];
        let task = FileSyncTask {
            rel_path: Path::new(file_name),
            src_path: &src_file_path,
            dest_path: &dst.join(file_name),
            dest_dir: &dst,
            src_size: 1536,
            src_mod,
            cached_id: None,
        };

        engine.sync_delta_large_file(&task, &mut scratch).unwrap();

        let dst_file = dst.join(file_name);
        assert!(dst_file.exists());
        assert_eq!(std::fs::metadata(&dst_file).unwrap().len(), 1536);
    }

    #[test]
    fn test_verification_mode_disabled() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let file_name = "small.txt";
        let src_file_path = src.join(file_name);
        std::fs::write(&src_file_path, b"hello world").unwrap();
        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file_path).unwrap()).unwrap();

        let target_cfg = TargetSyncConfig::builder(src.clone(), dst.clone())
            .verification_mode(VerificationMode::Disabled)
            .build()
            .unwrap();

        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        let task = FileSyncTask {
            rel_path: Path::new(file_name),
            src_path: &src_file_path,
            dest_path: &dst.join(file_name),
            dest_dir: &dst,
            src_size: 11,
            src_mod,
            cached_id: None,
        };

        let mut scratch = vec![0u8; 512];
        let res = engine.sync_small_file_core(&task, &mut scratch);
        assert!(res.is_ok());

        let dst_file = dst.join(file_name);
        assert!(dst_file.exists());
        assert_eq!(std::fs::read_to_string(&dst_file).unwrap(), "hello world");
    }

    #[test]
    fn test_sync_small_file_atomic_staging_no_partial_destination() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone());
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let dst_file = dst.join("target.txt");
        fs::write(&dst_file, b"ORIGINAL_DESTINATION_CONTENT").unwrap();

        // 1. Verify TempFileGuard behavior directly
        let tmp_path = dst.join("manual.syncdir_tmp");
        fs::write(&tmp_path, b"temporary data").unwrap();
        {
            let _guard = TempFileGuard::new(tmp_path.clone());
            assert!(tmp_path.exists());
        }
        assert!(
            !tmp_path.exists(),
            "Undisarmed TempFileGuard must remove file on drop"
        );

        let tmp_path2 = dst.join("disarmed.syncdir_tmp");
        fs::write(&tmp_path2, b"temporary data 2").unwrap();
        {
            let mut guard = TempFileGuard::new(tmp_path2.clone());
            guard.disarm();
        }
        assert!(
            tmp_path2.exists(),
            "Disarmed TempFileGuard must preserve file on drop"
        );
        fs::remove_file(&tmp_path2).unwrap();

        // 2. Failure scenario: pass a stale src_mod timestamp
        let src_file = src.join("target.txt");
        fs::write(&src_file, b"NEW_CONTENT_THAT_SHOULD_FAIL").unwrap();
        let task_failing = FileSyncTask {
            rel_path: Path::new("target.txt"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 28,
            src_mod: 999_999,
            cached_id: None,
        };
        let mut scratch = vec![0u8; 512];
        let fail_res = engine.sync_small_file_core(&task_failing, &mut scratch);
        assert!(
            fail_res.is_err(),
            "sync_small_file_core must fail when source mtime mismatch occurs"
        );
        assert_eq!(
            fs::read(&dst_file).unwrap(),
            b"ORIGINAL_DESTINATION_CONTENT",
            "Destination file must remain untouched when sync fails"
        );

        // 3. Success scenario
        let src_meta = fs::metadata(&src_file).unwrap();
        let task_valid = FileSyncTask {
            rel_path: Path::new("target.txt"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: src_meta.len() as i64,
            src_mod: safe_modified_millis(&src_meta).unwrap(),
            cached_id: None,
        };
        let success_res = engine.sync_small_file_core(&task_valid, &mut scratch);
        assert!(
            success_res.is_ok(),
            "sync_small_file_core should succeed with valid metadata: {success_res:?}"
        );
        assert_eq!(
            fs::read(&dst_file).unwrap(),
            b"NEW_CONTENT_THAT_SHOULD_FAIL",
            "Destination file must have updated content after successful staged rename"
        );

        // 4. Verify no .syncdir_tmp orphans remain
        let tmp_orphans: Vec<_> = fs::read_dir(&dst)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".syncdir_tmp"))
            .collect();
        assert_eq!(
            tmp_orphans.len(),
            0,
            "No .syncdir_tmp orphan files should remain in destination"
        );
    }
}

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::config::{TargetSyncConfig, VerificationMode};
use crate::db::FileRecord;
use crate::error::SyncError;

use super::types::{FileSyncTask, RelativePath, safe_epoch_duration_millis, safe_modified_millis};

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

#[inline]
pub(crate) fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

/// Dedicated collaborating engine for atomic small-file streaming and verification.
#[derive(Debug, Clone)]
pub(crate) struct SmallFileTransferEngine {
    config: TargetSyncConfig,
}

impl SmallFileTransferEngine {
    pub(crate) fn new(config: TargetSyncConfig) -> Self {
        Self { config }
    }

    fn copy_stream_and_hash(
        src_file: &mut File,
        temp_file: &mut File,
        buf: &mut [u8],
    ) -> Result<(u64, blake3::Hash), SyncError> {
        let mut hasher = blake3::Hasher::new();
        let mut total_bytes_copied: u64 = 0;
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
        Ok((total_bytes_copied, hasher.finalize()))
    }

    pub(crate) fn sync_small_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        if let Some(parent) = task.dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let counter = TEMP_FILE_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let seed = (std::process::id() as u64) ^ nanos ^ counter;
        let random_nonce = splitmix64(seed);

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

        let (total_bytes_copied, hash) = if scratch.len() >= 64 * 1024 {
            Self::copy_stream_and_hash(&mut src_file, &mut temp_file, &mut scratch[..64 * 1024])?
        } else {
            let mut stack_chunk = [0u8; 64 * 1024];
            Self::copy_stream_and_hash(&mut src_file, &mut temp_file, &mut stack_chunk)?
        };

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
            VerificationMode::MetadataAndFlush | VerificationMode::Sampled => {
                let temp_meta = temp_file.metadata()?;
                if temp_meta.len() != total_bytes_copied {
                    return Err(SyncError::write_verification_failed(
                        task.dest_path.to_path_buf(),
                    ));
                }
                temp_file.sync_all()?;
            }
            VerificationMode::Full => {
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
                if written_hasher.finalize() != hash {
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

        let rel = RelativePath::new(task.rel_path)?;
        let record =
            FileRecord::new(rel, total_bytes_copied, task.src_mod).with_optional_id(task.cached_id);
        tracing::info!(
            path = %task.rel_path.display(),
            target = %task.dest_dir.display(),
            size = total_bytes_copied,
            "Synced file to destination via atomic staging"
        );
        Ok((record, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
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

        let engine = SmallFileTransferEngine::new(target_cfg);

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
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = SmallFileTransferEngine::new(target_cfg);

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
            src_size: src_meta.len(),
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

    #[test]
    fn test_small_file_transfer_engine_standalone() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let cfg = test_config(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::from_config(&cfg, dest.clone()).unwrap();
        let engine = SmallFileTransferEngine::new(target_cfg);

        let src_file = source.join("hello.txt");
        let dst_file = dest.join("hello.txt");
        fs::write(&src_file, b"Hello standalone engine!").unwrap();

        let meta = fs::metadata(&src_file).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("hello.txt"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dest,
            src_size: meta.len(),
            src_mod: safe_modified_millis(&meta).unwrap(),
            cached_id: None,
        };

        let mut scratch = vec![0u8; 64 * 1024];
        let (record, block_hashes) = engine.sync_small_file_core(&task, &mut scratch).unwrap();
        assert_eq!(record.relative_path(), Path::new("hello.txt"));
        assert_eq!(record.file_size(), meta.len());
        assert!(block_hashes.is_empty());
        assert_eq!(fs::read(&dst_file).unwrap(), b"Hello standalone engine!");
    }

    #[test]
    fn test_small_file_sync_verification_mode_sampled_fast_path() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let target_cfg = TargetSyncConfig::builder(source.clone(), dest.clone())
            .verification_mode(crate::config::VerificationMode::Sampled)
            .build()
            .unwrap();
        let engine = SmallFileTransferEngine::new(target_cfg);

        let src_file = source.join("test.txt");
        let dst_file = dest.join("test.txt");
        let content = b"Sampled verification mode content";
        fs::write(&src_file, content).unwrap();

        let meta = fs::metadata(&src_file).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("test.txt"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dest,
            src_size: meta.len(),
            src_mod: safe_modified_millis(&meta).unwrap(),
            cached_id: None,
        };

        let mut scratch = vec![0u8; 64 * 1024];
        let (record, hashes) = engine.sync_small_file_core(&task, &mut scratch).unwrap();
        assert_eq!(record.file_size(), content.len() as u64);
        assert!(hashes.is_empty());
        assert_eq!(fs::read(&dst_file).unwrap(), content);
    }

    #[test]
    fn test_sync_small_file_staging_nonce_and_buffer_fallback() {
        use crate::config::{TargetSyncConfig, VerificationMode};
        use crate::sync::small_file::SmallFileTransferEngine;
        use crate::sync::types::{FileSyncTask, safe_modified_millis};
        use std::fs;
        use std::path::Path;
        use tempfile::tempdir;

        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let file_name = "payload.dat";
        let src_file = src.join(file_name);
        let test_data = vec![0x42u8; 128 * 1024];
        fs::write(&src_file, &test_data).unwrap();

        let meta = fs::metadata(&src_file).unwrap();
        let src_mod = safe_modified_millis(&meta).unwrap();

        let target_cfg = TargetSyncConfig::builder(src.clone(), dst.clone())
            .verification_mode(VerificationMode::Full)
            .build()
            .unwrap();
        let engine = SmallFileTransferEngine::new(target_cfg);

        let dest_file = dst.join(file_name);
        let task = FileSyncTask {
            rel_path: Path::new(file_name),
            src_path: &src_file,
            dest_path: &dest_file,
            dest_dir: &dst,
            src_size: test_data.len() as u64,
            src_mod,
            cached_id: None,
        };

        let mut tiny_scratch = vec![0u8; 16];
        let (rec, hashes) = engine
            .sync_small_file_core(&task, &mut tiny_scratch)
            .expect("Small file transfer must fall back to stack buffer when scratch buffer is undersized");

        assert_eq!(rec.file_size(), test_data.len() as u64);
        assert!(hashes.is_empty());

        assert!(dest_file.exists());
        assert_eq!(fs::read(&dest_file).unwrap(), test_data);

        let lingering_tmp: Vec<_> = fs::read_dir(&dst)
            .unwrap()
            .filter_map(|e| {
                e.ok()
                    .map(|de| de.file_name().to_string_lossy().into_owned())
            })
            .filter(|name| name.ends_with(".syncdir_tmp"))
            .collect();
        assert!(lingering_tmp.is_empty());
    }
}

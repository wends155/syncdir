//! Domain types and task representations for synchronization operations.

use crate::error::SyncError;
pub use crate::path_util::RelativePath;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

/// A task representing a single file synchronization operation.
#[derive(Debug, Clone)]
pub struct FileSyncTask<'a> {
    pub rel_path: &'a Path,
    pub src_path: &'a Path,
    pub dest_path: &'a Path,
    pub dest_dir: &'a Path,
    pub src_size: u64,
    pub src_mod: i64,
    pub cached_id: Option<i64>,
}

impl<'a> FileSyncTask<'a> {
    pub fn new(
        rel_path: &'a Path,
        src_path: &'a Path,
        dest_path: &'a Path,
        dest_dir: &'a Path,
        src_size: u64,
        src_mod: i64,
        cached_id: Option<i64>,
    ) -> Self {
        Self {
            rel_path,
            src_path,
            dest_path,
            dest_dir,
            src_size,
            src_mod,
            cached_id,
        }
    }
}

/// Extract file modified time as milliseconds since UNIX epoch.
///
/// Pre-1970 timestamps are silently clamped to 0 (epoch).
///
/// # Errors
///
/// Returns `SyncError::Io` if the file's modified time cannot be read.
pub fn safe_modified_millis(metadata: &std::fs::Metadata) -> Result<i64, SyncError> {
    let modified = metadata.modified().map_err(SyncError::Io)?;
    match modified.duration_since(UNIX_EPOCH) {
        Ok(dur) => Ok(dur.as_millis() as i64),
        Err(_) => Ok(0),
    }
}

/// Convert a millisecond timestamp to a `Duration`, clamping negative values to zero.
#[must_use]
pub fn safe_epoch_duration_millis(millis: i64) -> Duration {
    Duration::from_millis(millis.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_sync_task_construction_and_timestamp_helpers() {
        let src = std::path::Path::new("C:\\src\\file.txt");
        let dest = std::path::Path::new("C:\\dst\\file.txt");
        let dest_dir = std::path::Path::new("C:\\dst");
        let rel = std::path::Path::new("file.txt");
        let task = FileSyncTask::new(rel, src, dest, dest_dir, 1024, 1700000000, Some(42));
        assert_eq!(task.src_size, 1024);
        assert_eq!(task.src_mod, 1700000000);
        assert_eq!(task.cached_id, Some(42));
        assert_eq!(task.rel_path, rel);

        assert_eq!(
            safe_epoch_duration_millis(5500),
            std::time::Duration::from_millis(5500)
        );
        assert_eq!(
            safe_epoch_duration_millis(-500),
            std::time::Duration::from_millis(0)
        );
    }

    #[test]
    fn test_safe_modified_millis_no_side_effect_logs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("pre_epoch.txt");
        std::fs::write(&test_file, b"test").unwrap();

        let file = std::fs::File::options()
            .write(true)
            .open(&test_file)
            .unwrap();
        let pre_epoch = std::time::UNIX_EPOCH - std::time::Duration::from_secs(86400 * 365);
        let times = std::fs::FileTimes::new().set_modified(pre_epoch);
        file.set_times(times).unwrap();
        drop(file);

        let meta = std::fs::metadata(&test_file).unwrap();
        let (res, log_output) =
            crate::test_support::with_captured_tracing(|| safe_modified_millis(&meta));
        assert_eq!(res.unwrap(), 0);
        assert!(
            log_output.is_empty(),
            "Expected zero log records emitted, but got: {log_output}"
        );
    }
}

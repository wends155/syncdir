use insta::assert_snapshot;
use std::path::PathBuf;
use syncdir::config::Config;
use syncdir::db::FileRecord;
use syncdir::error::SyncError;

// --- Config Parsing Snapshots ---

#[test]
fn test_config_snapshot_basic() {
    let config = Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"D:\dest"));
    assert_snapshot!(format!("{:#?}", config));
}

#[test]
fn test_config_snapshot_multi_dest() {
    let config = Config::builder(PathBuf::from(r"C:\source"))
        .dest_dir(PathBuf::from(r"D:\Backup1"))
        .dest_dirs(vec![
            PathBuf::from(r"D:\Backup1"),
            PathBuf::from(r"E:\Backup2"),
            PathBuf::from(r"\\172.16.0.60\scada_data"),
        ])
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build();
    assert_snapshot!(format!("{:#?}", config));
}

// --- Validation Error Snapshots ---

#[test]
fn test_config_validation_error_not_a_directory() {
    let config = Config::test_default(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        PathBuf::from(r"D:\dest"),
    );
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_no_dests() {
    let config = Config::builder(PathBuf::from(r"C:\source"))
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build();
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_zero_debounce() {
    let config = Config::builder(PathBuf::from(r"C:\source"))
        .dest_dir(PathBuf::from(r"D:\dest"))
        .debounce_seconds(0)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build();
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_invalid_relative_source() {
    let config = Config::builder(PathBuf::from("relative/source"))
        .dest_dir(PathBuf::from(r"D:\dest"))
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build();
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_zero_block_size() {
    let config = Config::builder(PathBuf::from(r"C:\source"))
        .dest_dir(PathBuf::from(r"D:\dest"))
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(0)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build();
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_zero_threshold() {
    let config = Config::builder(PathBuf::from(r"C:\source"))
        .dest_dir(PathBuf::from(r"D:\dest"))
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(0)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build();
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

// --- SyncError Display Snapshots ---

#[test]
fn test_sync_error_display_io() {
    let err = SyncError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "file not found",
    ));
    assert_snapshot!(err.to_string());
}

#[test]
fn test_sync_error_display_config() {
    let err = SyncError::config("invalid TOML: missing field 'source_dir'");
    assert_snapshot!(err.to_string());
}

#[test]
fn test_sync_error_display_validation() {
    let err = SyncError::Validation("source directory does not exist".to_string());
    assert_snapshot!(err.to_string());
}

#[test]
fn test_sync_error_display_tray() {
    let err = SyncError::Tray("Failed to create tray icon".to_string());
    assert_snapshot!(err.to_string());
}

#[test]
fn test_sync_error_display_registry() {
    let err = SyncError::Registry("Failed to open Run registry key".to_string());
    assert_snapshot!(err.to_string());
}

// --- FileRecord Debug Snapshot ---

#[test]
fn test_file_record_snapshot() {
    let record = FileRecord {
        id: Some(42),
        relative_path: PathBuf::from("docs/readme.txt"),
        file_size: 8192,
        last_modified: 1722470400,
    };
    assert_snapshot!(format!("{:#?}", record));
}

// --- Log Formatting ANSI Suppression Test ---

#[test]
fn test_log_formatter_ansi_suppression() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let buffer = SharedBuffer::default();
    let writer_buffer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer_buffer.clone())
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(test_key = "test_val", "ANSI suppression verification");
    });

    let bytes = buffer.0.lock().unwrap().clone();
    let log_str = String::from_utf8_lossy(&bytes);

    assert!(log_str.contains("ANSI suppression verification"));
    assert!(
        !log_str.contains('\x1b'),
        "Log string contained ANSI escape character: {}",
        log_str
    );
}

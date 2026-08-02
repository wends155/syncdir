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
    let mut config =
        Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"D:\Backup1"));
    config.dest_dirs = Some(vec![
        PathBuf::from(r"D:\Backup1"),
        PathBuf::from(r"E:\Backup2"),
        PathBuf::from(r"\\172.16.0.60\scada_data"),
    ]);
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
    let mut config = Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"D:\dest"));
    config.dest_dir = None;
    config.dest_dirs = None;
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_zero_debounce() {
    let mut config = Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"D:\dest"));
    config.debounce_seconds = 0;
    let err = config.validate().unwrap_err();
    assert_snapshot!(err.to_string());
}

#[test]
fn test_config_validation_error_invalid_relative_source() {
    let mut config = Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"D:\dest"));
    config.source_dir = PathBuf::from("relative/source");
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
    let err = SyncError::Config("invalid TOML: missing field 'source_dir'".to_string());
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

// --- FileRecord Debug Snapshot ---

#[test]
fn test_file_record_snapshot() {
    let record = FileRecord {
        id: Some(42),
        relative_path: "docs/readme.txt".to_string(),
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

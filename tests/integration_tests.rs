use std::path::PathBuf;
use syncdir::config::Config;
use syncdir::db::{FileRecord, HashStore, SqliteHashStore};
use syncdir::sync::SyncCommand;
use tempfile::{tempdir, NamedTempFile};

#[test]
fn test_integration_config_db_sync_commands() {
    let dir = tempdir().unwrap();
    let db_file = NamedTempFile::new().unwrap();

    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let dest = dir.path().join("dest");
    std::fs::create_dir(&dest).unwrap();

    let config = Config {
        source_dir: source,
        dest_dir: dest,
        debounce_seconds: 5,
        propagate_deletions: false,
        block_sync_threshold_bytes: 4096,
        block_size_bytes: 1024,
        verify_writes: true,
    };

    assert!(config.validate().is_ok());

    // SyncCommand type-checks and equality
    let cmd = SyncCommand::FileModified(PathBuf::from("test.txt"));
    assert_eq!(cmd, SyncCommand::FileModified(PathBuf::from("test.txt")));
    assert_ne!(cmd, SyncCommand::TriggerFullScan);

    // Database round-trip
    let store = SqliteHashStore::new(db_file.path(), &config).unwrap();
    let record = FileRecord {
        id: None,
        relative_path: "test_file.bin".to_string(),
        file_size: 4096,
        last_modified: 99999,
    };
    let hashes = vec![vec![9; 32]; 4];

    store.save_file(&record, &hashes).unwrap();
    let fetched = store.get_file("test_file.bin").unwrap().unwrap();
    assert_eq!(fetched.file_size, 4096);

    let fetched_hashes = store.get_block_hashes(fetched.id.unwrap()).unwrap();
    assert_eq!(fetched_hashes.len(), 4);
}

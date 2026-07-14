use std::path::PathBuf;
use syncdir::config::Config;
use syncdir::db::{FileRecord, HashStore, SqliteHashStore};
use syncdir::sync::SyncCommand;
use tempfile::{NamedTempFile, tempdir};

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

#[test]
fn test_watcher_and_sync_engine_flow() {
    use std::sync::mpsc::channel;
    use syncdir::monitor::DirectoryWatcher;
    use syncdir::sync::start_sync_worker;

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");

    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config {
        source_dir: source.clone(),
        dest_dir: dest.clone(),
        debounce_seconds: 1,
        propagate_deletions: true,
        block_sync_threshold_bytes: 10,
        block_size_bytes: 4,
        verify_writes: true,
    };

    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let (tx, rx) = channel();

    // Start watcher & sync worker BEFORE writing the file
    let _watcher = DirectoryWatcher::start(&config, tx).unwrap();
    let _worker_handle = start_sync_worker(config.clone(), store, rx);

    // Write a file in source — watcher should pick it up
    let file_path = source.join("notes.txt");
    std::fs::write(&file_path, b"hello world").unwrap();

    // Wait for debounce (1s) + processing margin
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Verify dest file contains synchronized content
    let dest_file_path = dest.join("notes.txt");
    assert!(
        dest_file_path.exists(),
        "Destination file should exist after sync"
    );
    let content = std::fs::read_to_string(&dest_file_path).unwrap();
    assert_eq!(content, "hello world");
}

#[test]
fn test_tray_module_compiles() {
    // Since run_tray blocks the thread, we only smoke-test compiling it and verifying exports.
    // This is a static analysis verification.
    let _func: fn(
        std::path::PathBuf,
        std::path::PathBuf,
        std::sync::mpsc::Sender<syncdir::sync::SyncCommand>,
    ) -> Result<(), syncdir::error::SyncError> = syncdir::tray::run_tray;
}

#[test]
fn test_propagate_deletions_false() {
    use syncdir::sync::{LocalSyncEngine, SyncEngine};

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config {
        source_dir: source.clone(),
        dest_dir: dest.clone(),
        debounce_seconds: 1,
        propagate_deletions: false,
        block_sync_threshold_bytes: 10,
        block_size_bytes: 4,
        verify_writes: true,
    };

    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let engine = LocalSyncEngine::new(store, config);

    let file_path = source.join("test.txt");
    std::fs::write(&file_path, b"hello").unwrap();
    engine.sync_file("test.txt").unwrap();

    // Verify file exists on destination
    let dest_path = dest.join("test.txt");
    assert!(dest_path.exists());

    // Call delete_file
    engine.delete_file("test.txt").unwrap();

    // Since propagate_deletions = false, the destination file MUST remain
    assert!(
        dest_path.exists(),
        "Destination file must not be deleted when propagate_deletions is false"
    );
}

#[test]
fn test_watcher_rename_event() {
    use std::sync::mpsc::channel;
    use syncdir::monitor::DirectoryWatcher;
    use syncdir::sync::SyncCommand;

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config {
        source_dir: source.clone(),
        dest_dir: dest.clone(),
        debounce_seconds: 1,
        propagate_deletions: true,
        block_sync_threshold_bytes: 10,
        block_size_bytes: 4,
        verify_writes: true,
    };

    let (tx, rx) = channel();
    let _watcher = DirectoryWatcher::start(&config, tx).unwrap();

    let old_file = source.join("old.txt");
    std::fs::write(&old_file, b"test").unwrap();

    // Wait for creation event to clear
    std::thread::sleep(std::time::Duration::from_millis(500));
    while rx.try_recv().is_ok() {}

    let new_file = source.join("new.txt");
    std::fs::rename(&old_file, &new_file).unwrap();

    // Wait for rename events
    std::thread::sleep(std::time::Duration::from_secs(2));

    let mut received = Vec::new();
    while let Ok(cmd) = rx.try_recv() {
        received.push(cmd);
    }

    assert!(
        received.contains(&SyncCommand::FileDeleted(PathBuf::from("old.txt"))),
        "Should receive deletion command for renamed-from file, got: {:?}",
        received
    );
    assert!(
        received.contains(&SyncCommand::FileModified(PathBuf::from("new.txt"))),
        "Should receive modification command for renamed-to file, got: {:?}",
        received
    );
}

use pretty_assertions::assert_eq;
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

    let config = Config::builder(source)
        .dest_dir(dest)
        .debounce_seconds(5)
        .propagate_deletions(false)
        .block_sync_threshold_bytes(4096)
        .block_size_bytes(1024)
        .build();

    assert!(config.validate().is_ok());

    // SyncCommand type-checks and equality
    let cmd = SyncCommand::FileModified(PathBuf::from("test.txt"));
    assert_eq!(cmd, SyncCommand::FileModified(PathBuf::from("test.txt")));
    assert_ne!(cmd, SyncCommand::TriggerFullScan);

    // Database round-trip
    let store = SqliteHashStore::new(db_file.path(), &config).unwrap();
    let record = FileRecord {
        id: None,
        relative_path: PathBuf::from("test_file.bin"),
        file_size: 4096,
        last_modified: 99999,
    };
    let hashes = vec![[9u8; 32]; 4];

    store.save_file(&record, &hashes).unwrap();
    let fetched = store
        .get_file(std::path::Path::new("test_file.bin"))
        .unwrap()
        .unwrap();
    assert_eq!(fetched.file_size, 4096);

    let fetched_hashes = store
        .get_block_hashes(std::path::Path::new("test_file.bin"))
        .unwrap();
    assert_eq!(fetched_hashes.len(), 4);
}

#[test]
fn test_watcher_and_sync_engine_flow() {
    use std::sync::mpsc::channel;
    use syncdir::config::TargetSyncConfig;
    use syncdir::monitor::DirectoryWatcher;
    use syncdir::sync::{LocalSyncEngine, SyncWorkerContext, start_sync_worker};

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");

    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());

    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let (tx, rx) = channel();

    // Start watcher & sync worker BEFORE writing the file
    let _watcher = DirectoryWatcher::start(&source, tx.clone()).unwrap();
    let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let target_config = TargetSyncConfig::from_config(&config, dest.clone());
    let engine = LocalSyncEngine::new(store, target_config.clone());
    let worker_ctx = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
    let _worker_handle = start_sync_worker(worker_ctx);

    // Give watcher thread time to establish OS directory hook and catch-up scan to stabilize
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Write a file in source — watcher should pick it up
    let file_path = source.join("notes.txt");
    std::fs::write(&file_path, b"hello world").unwrap();

    // Wait for debounce (1s) and sync to complete (allow up to 5s under load)
    let dest_file_path = dest.join("notes.txt");
    let start = std::time::Instant::now();
    let mut content = String::new();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        if let Ok(c) = std::fs::read_to_string(&dest_file_path)
            && c == "hello world"
        {
            content = c;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        content, "hello world",
        "Destination file should contain synchronized content after debounce"
    );
}

#[test]
#[allow(clippy::type_complexity)]
fn test_tray_module_compiles() {
    // Since run_tray blocks the thread, we only smoke-test compiling it and verifying exports.
    // This is a static analysis verification.
    struct DummyHandler;
    impl syncdir::tray::TrayActionHandler for DummyHandler {
        fn on_sync_now(&self) -> Result<(), syncdir::error::SyncError> {
            Ok(())
        }
        fn on_reload_config(&self) -> Result<(), syncdir::error::SyncError> {
            Ok(())
        }
        fn on_toggle_startup(&self, _enable: bool) -> Result<bool, syncdir::error::SyncError> {
            Ok(false)
        }
        fn is_startup_enabled(&self) -> Result<bool, syncdir::error::SyncError> {
            Ok(false)
        }
    }

    let _func: fn(
        winit::event_loop::EventLoop<syncdir::tray::UserEvent>,
        std::vec::Vec<syncdir::tray::DestinationState>,
        std::sync::Arc<DummyHandler>,
    ) -> Result<syncdir::tray::TrayExitReason, syncdir::error::SyncError> =
        syncdir::tray::run_tray::<DummyHandler>;
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

    let config = Config::builder(source.clone())
        .dest_dir(dest.clone())
        .propagate_deletions(false)
        .build();

    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let engine = LocalSyncEngine::new(store, config);

    let file_path = source.join("test.txt");
    std::fs::write(&file_path, b"hello").unwrap();
    engine.sync_file(std::path::Path::new("test.txt")).unwrap();

    // Verify file exists on destination
    let dest_path = dest.join("test.txt");
    assert!(dest_path.exists());

    // Call delete_file
    engine
        .delete_file(std::path::Path::new("test.txt"))
        .unwrap();

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

    let _config = Config::test_default(source.clone(), dest.clone());

    let (tx, rx) = channel();
    let _watcher = DirectoryWatcher::start(&source, tx).unwrap();

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

#[test]
fn test_path_traversal_prevention() {
    use syncdir::sync::{LocalSyncEngine, SyncEngine};
    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());
    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let engine = LocalSyncEngine::new(store, config);

    // Absolute path
    let res1 = engine.sync_file(std::path::Path::new("/etc/passwd"));
    assert!(matches!(
        res1,
        Err(syncdir::error::SyncError::Validation(_))
    ));

    // Traversal path
    let res2 = engine.sync_file(std::path::Path::new("../test.txt"));
    assert!(matches!(
        res2,
        Err(syncdir::error::SyncError::Validation(_))
    ));
}

#[test]
fn test_subsecond_sync_precision() {
    use syncdir::sync::{LocalSyncEngine, SyncEngine};
    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());
    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let engine = LocalSyncEngine::new(store, config);

    let file_path = source.join("fast.txt");
    std::fs::write(&file_path, b"initial").unwrap();
    engine.sync_file(std::path::Path::new("fast.txt")).unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("fast.txt")).unwrap(),
        "initial"
    );

    // Write a second time immediately
    std::fs::write(&file_path, b"updated").unwrap();
    engine.sync_file(std::path::Path::new("fast.txt")).unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("fast.txt")).unwrap(),
        "updated"
    );
}

#[test]
fn test_directory_rename_syncs_child_files() {
    use syncdir::sync::{LocalSyncEngine, SyncEngine};

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());
    let store = SqliteHashStore::new(&db_path, &config).unwrap();
    let engine = LocalSyncEngine::new(store, config);

    // Create a folder with child files
    let sub = source.join("folder");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("file1.txt"), b"child 1").unwrap();
    let nested = sub.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("file2.txt"), b"child 2").unwrap();

    // sync_file on directory path should recursively sync child files
    engine.sync_file(std::path::Path::new("folder")).unwrap();

    assert!(dest.join("folder").join("file1.txt").exists());
    assert_eq!(
        std::fs::read(dest.join("folder").join("file1.txt")).unwrap(),
        b"child 1"
    );
    assert!(
        dest.join("folder")
            .join("nested")
            .join("file2.txt")
            .exists()
    );
    assert_eq!(
        std::fs::read(dest.join("folder").join("nested").join("file2.txt")).unwrap(),
        b"child 2"
    );
}

#[test]
fn test_reload_config_validation_error() {
    // Test that Config::load fails on invalid TOML
    let dir = tempdir().unwrap();
    let bad_config = dir.path().join("bad_config.toml");
    std::fs::write(&bad_config, "this is not valid toml [[[").unwrap();
    assert!(Config::load(&bad_config).is_err());

    // Test that Config::validate fails when no destinations are configured
    let no_dest_config = dir.path().join("no_dest.toml");
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        &no_dest_config,
        format!(
            r#"source_dir = "{}"
debounce_seconds = 3
propagate_deletions = true
block_sync_threshold_bytes = 1024
block_size_bytes = 512
verify_writes = true
"#,
            source.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let config = Config::load(&no_dest_config).unwrap();
    assert!(
        config.validate().is_err(),
        "Config with no destinations should fail validation"
    );
}

#[test]
fn test_sync_daemon_shutdown_order() {
    use syncdir::daemon::SyncDaemon;

    let dir = tempdir().unwrap();
    let src = dir.path().join("source");
    let dst = dir.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();

    let config = Config::builder(src).dest_dir(dst).build();
    let daemon = SyncDaemon::start(config, dir.path(), None).unwrap();
    daemon.shutdown();
}

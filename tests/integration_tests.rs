use pretty_assertions::assert_eq;
use syncdir::config::{Config, TargetDir, TargetSyncConfig};
use syncdir::db::{FileRecord, HashStore, SqliteHashStore, StoreConfig};
use syncdir::path_util::RelativePath;
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
        .debounce_seconds(1)
        .retry_interval_seconds(1)
        .build()
        .unwrap();

    assert!(config.validate().is_ok());

    let (tx, rx) = std::sync::mpsc::channel();
    let cmd = SyncCommand::FileModified(RelativePath::new("test.txt").unwrap());
    tx.send(cmd.clone()).unwrap();

    let received = rx.recv().unwrap();
    assert_eq!(received, cmd);
    assert_eq!(
        cmd,
        SyncCommand::FileModified(RelativePath::new("test.txt").unwrap())
    );
    assert_ne!(cmd, SyncCommand::TriggerFullScan);

    // Database round-trip
    let store = SqliteHashStore::new(
        db_file.path(),
        StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let record = FileRecord::from_raw("test_file.bin", 4096, 99999).unwrap();
    let hashes = vec![[9u8; 32]; 4];

    store.save_file(&record, &hashes).unwrap();
    let fetched = store
        .get_file(std::path::Path::new("test_file.bin"))
        .unwrap()
        .unwrap();
    assert_eq!(fetched.file_size(), 4096);

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

    let store = SqliteHashStore::new(
        &db_path,
        StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let (tx, rx) = channel();

    // Start watcher & sync worker BEFORE writing the file
    let _watcher = DirectoryWatcher::start(&source, tx.clone()).unwrap();
    let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let target_config =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(dest.clone())).unwrap();
    let engine = LocalSyncEngine::new(store, target_config.clone());
    let worker_ctx = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
    let _worker_handle = start_sync_worker(worker_ctx).unwrap();

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

type TrayLoopRunner<H> = fn(
    syncdir::tray::TrayEventLoop,
    std::vec::Vec<syncdir::tray::DestinationState>,
    std::sync::Arc<H>,
) -> Result<syncdir::tray::TrayExitReason, syncdir::error::SyncError>;

#[test]
fn test_tray_module_compiles() {
    // Since TrayEventLoop::run blocks the thread, we only smoke-test compiling it and verifying exports.
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

    let _func: TrayLoopRunner<DummyHandler> = syncdir::tray::TrayEventLoop::run::<DummyHandler>;
}

#[test]
fn test_propagate_deletions_false() {
    use syncdir::sync::LocalSyncEngine;

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::builder(source.clone())
        .dest_dir(dest.clone())
        .propagate_deletions(false)
        .build()
        .unwrap();
    let target_cfg =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(dest.clone())).unwrap();

    let store = SqliteHashStore::new(
        &db_path,
        StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let engine = LocalSyncEngine::new(store, target_cfg);

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
        received.contains(&SyncCommand::FileDeleted(
            RelativePath::new("old.txt").unwrap()
        )),
        "Should receive deletion command for renamed-from file, got: {:?}",
        received
    );
    assert!(
        received.contains(&SyncCommand::FileModified(
            RelativePath::new("new.txt").unwrap()
        )),
        "Should receive modification command for renamed-to file, got: {:?}",
        received
    );
}

#[test]
fn test_path_traversal_prevention() {
    use syncdir::sync::LocalSyncEngine;
    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());
    let target_cfg =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(dest.clone())).unwrap();
    let store = SqliteHashStore::new(
        &db_path,
        StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let engine = LocalSyncEngine::new(store, target_cfg);

    // Absolute path
    let res1 = engine.sync_file(std::path::Path::new("/etc/passwd"));
    assert!(matches!(
        res1,
        Err(syncdir::error::SyncError::Validation { .. })
    ));

    // Traversal path
    let res2 = engine.sync_file(std::path::Path::new("../test.txt"));
    assert!(matches!(
        res2,
        Err(syncdir::error::SyncError::Validation { .. })
    ));
}

#[test]
fn test_subsecond_sync_precision() {
    use syncdir::sync::LocalSyncEngine;
    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());
    let target_cfg =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(dest.clone())).unwrap();
    let store = SqliteHashStore::new(
        &db_path,
        StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let engine = LocalSyncEngine::new(store, target_cfg);

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
    use syncdir::sync::LocalSyncEngine;

    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    let db_path = dir.path().join("sigcache.db");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();

    let config = Config::test_default(source.clone(), dest.clone());
    let target_cfg =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(dest.clone())).unwrap();
    let store = SqliteHashStore::new(
        &db_path,
        StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    let engine = LocalSyncEngine::new(store, target_cfg);

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

    let config = Config::builder(src).dest_dir(dst).build().unwrap();
    let daemon = SyncDaemon::start(config, dir.path(), None).unwrap();
    daemon.shutdown();
}

#[test]
fn test_worker_reachability_and_offline_drain_guard() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::channel;
    use std::time::Duration;
    use syncdir::net::MockNetworkResolver;
    use syncdir::sync::{MockSyncEngine, SyncWorkerContext, start_sync_worker};

    let dir = tempdir().unwrap();
    let src = dir.path().join("source");
    let dst = dir.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();

    let config = Config::builder(src)
        .dest_dir(dst.clone())
        .debounce_seconds(1)
        .retry_interval_seconds(1)
        .build()
        .unwrap();
    let target_cfg =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(dst.clone())).unwrap();

    let mock_engine = MockSyncEngine::new();
    let mock_resolver = Arc::new(MockNetworkResolver::new());
    // Initially mark destination inaccessible via mock resolver
    mock_resolver.set_destination_accessible(false);

    let (tx, rx) = channel();
    let cancellation = Arc::new(AtomicBool::new(false));

    let context = SyncWorkerContext::new(0, target_cfg, mock_engine.clone(), rx, None, true)
        .with_resolver(mock_resolver.clone())
        .with_cancellation(cancellation.clone());

    let handle = start_sync_worker(context).unwrap();

    // Send a sync command while offline
    tx.send(SyncCommand::FileModified(
        RelativePath::new("offline_test.txt").unwrap(),
    ))
    .unwrap();

    // Sleep briefly to allow worker loop tick
    std::thread::sleep(Duration::from_millis(200));

    // Because resolver said destination was inaccessible, mock_engine must NOT have synced
    assert_eq!(
        mock_engine.synced_calls().len(),
        0,
        "Worker must not attempt sync while resolver indicates destination is inaccessible"
    );

    // Now restore accessibility via resolver
    mock_resolver.set_destination_accessible(true);

    // Wait for reachability check interval and drain
    let start = std::time::Instant::now();
    while mock_engine.synced_calls().is_empty() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        !mock_engine.synced_calls().is_empty(),
        "Worker must sync file once destination becomes accessible via resolver"
    );

    cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = handle.join();
}

#[test]
fn test_delta_sync_interrupted_write_invalidates_cache() {
    use std::path::Path;
    use syncdir::config::{Config, TargetDir, TargetSyncConfig};
    use syncdir::db::{HashStore, SqliteHashStore, StoreConfig};
    use syncdir::sync::LocalSyncEngine;

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LockFile(
                hFile: std::os::windows::raw::HANDLE,
                dwFileOffsetLow: u32,
                dwFileOffsetHigh: u32,
                nNumberOfBytesToLockLow: u32,
                nNumberOfBytesToLockHigh: u32,
            ) -> i32;
            fn UnlockFile(
                hFile: std::os::windows::raw::HANDLE,
                dwFileOffsetLow: u32,
                dwFileOffsetHigh: u32,
                nNumberOfBytesToUnlockLow: u32,
                nNumberOfBytesToUnlockHigh: u32,
            ) -> i32;
        }

        let dir = tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        let db_path = dir.path().join("sigcache.db");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(512)
            .build()
            .unwrap();

        let store = SqliteHashStore::new(
            &db_path,
            StoreConfig::new(
                config.block_size_bytes(),
                config.block_sync_threshold_bytes(),
            )
            .unwrap(),
        )
        .unwrap();

        let rel_path = Path::new("large.bin");
        let src_file = src.join("large.bin");
        let dst_file = dst.join("large.bin");

        // Create 2048-byte files (4 blocks of 512 bytes)
        std::fs::write(&src_file, vec![1u8; 2048]).unwrap();
        std::fs::write(&dst_file, vec![1u8; 2048]).unwrap();

        let target_cfg =
            TargetSyncConfig::from_config(&config, TargetDir::from_validated(dst.clone())).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // Initial sync populates SQLite cache
        engine.sync_file(rel_path).unwrap();

        let verify_store_initial = SqliteHashStore::new(
            &db_path,
            StoreConfig::new(
                config.block_size_bytes(),
                config.block_sync_threshold_bytes(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            verify_store_initial.get_file(rel_path).unwrap().is_some(),
            "Record must be present in SQLite cache after initial sync"
        );

        // Modify source file to have differing blocks
        std::fs::write(&src_file, vec![2u8; 2048]).unwrap();

        // Lock byte range [1024..2048] of the destination file using Windows LockFile
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&dst_file)
            .unwrap();
        let lock_success = unsafe { LockFile(lock_file.as_raw_handle(), 1024, 0, 1024, 0) };
        assert_ne!(lock_success, 0, "LockFile must succeed");

        // Sync will start writing dirty block 0, but fail writing dirty block 1 due to lock violation
        let sync_res = engine.sync_file(rel_path);
        assert!(
            sync_res.is_err(),
            "Sync must fail when destination file block is locked"
        );

        // Unlock the file
        unsafe {
            UnlockFile(lock_file.as_raw_handle(), 1024, 0, 1024, 0);
        }
        drop(lock_file);

        // Because dirty blocks were written before the failure, SQLite cache record must be deleted
        let verify_store_after = SqliteHashStore::new(
            &db_path,
            StoreConfig::new(
                config.block_size_bytes(),
                config.block_sync_threshold_bytes(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            verify_store_after.get_file(rel_path).unwrap().is_none(),
            "SQLite cache record must be invalidated (deleted) after interrupted delta sync"
        );
    }
}

#[test]
fn test_consumer_accepts_segregated_traits() {
    use std::path::Path;
    use syncdir::sync::{FileDeleter, FileSynchronizer, MockSyncEngine};

    fn sync_and_delete(syncer: &dyn FileSynchronizer, deleter: &dyn FileDeleter, file: &Path) {
        syncer.sync_file(file).unwrap();
        deleter.delete_file(file).unwrap();
    }

    let engine = MockSyncEngine::new();
    sync_and_delete(&engine, &engine, Path::new("test.txt"));
    assert_eq!(engine.synced_calls().len(), 1);
    assert_eq!(engine.deleted_calls().len(), 1);
}

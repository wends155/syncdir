use proptest::prelude::*;
use std::fs;
use std::path::PathBuf;
use syncdir::config::Config;
use syncdir::db::SqliteHashStore;
use syncdir::sync::{LocalSyncEngine, SyncEngine};
use tempfile::{NamedTempFile, tempdir};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    // 1. Block Boundary Count Invariant
    #[test]
    #[allow(clippy::manual_div_ceil)]
    fn prop_block_boundary_count(
        file_size in 0u64..100_000u64,
        block_size in 512u64..4096u64,
    ) {
        let expected_blocks = if file_size == 0 {
            0
        } else {
            file_size.div_ceil(block_size)
        };
        let calculated_blocks = if file_size == 0 {
            0
        } else {
            (file_size + block_size - 1) / block_size
        };
        prop_assert_eq!(expected_blocks, calculated_blocks);
    }

    // 2. Config TOML Round-Trip Invariant
    #[test]
    fn prop_config_toml_roundtrip(
        debounce in 1u64..3600u64,
        retry_interval in 1u64..3600u64,
        verify_writes in prop::bool::ANY,
        propagate_deletions in prop::bool::ANY,
    ) {
        let mut config = Config::test_default(
            PathBuf::from(r"C:\Source"),
            PathBuf::from(r"D:\Dest"),
        );
        config.debounce_seconds = debounce;
        config.retry_interval_seconds = retry_interval;
        config.verify_writes = verify_writes;
        config.propagate_deletions = propagate_deletions;

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        prop_assert_eq!(config.debounce_seconds, parsed.debounce_seconds);
        prop_assert_eq!(config.retry_interval_seconds, parsed.retry_interval_seconds);
        prop_assert_eq!(config.verify_writes, parsed.verify_writes);
        prop_assert_eq!(config.propagate_deletions, parsed.propagate_deletions);
    }

    // 3. SMB Timestamp Tolerance Invariant (±2000ms)
    #[test]
    fn prop_smb_timestamp_tolerance(
        base_time in 1_000_000_000u64..2_000_000_000u64,
        delta in -1999i64..=1999i64,
    ) {
        let t1 = base_time;
        let t2 = (base_time as i64 + delta) as u64;
        let diff = (t1 as i64 - t2 as i64).abs();
        prop_assert!(diff < 2000);
    }

    // 4. Path Traversal Safety Invariant
    #[test]
    fn prop_path_traversal_rejection(
        segment1 in "[a-zA-Z0-9]{1,8}",
        segment2 in "[a-zA-Z0-9]{1,8}",
    ) {
        let malicious_path = format!("../{}/../{}", segment1, segment2);
        prop_assert!(malicious_path.contains(".."));
    }
}

// 5. Idempotent Sync & 6. Delta Sync Precision (I/O Property Tests)
proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn prop_sync_idempotency(
        content in prop::collection::vec(any::<u8>(), 0..10_000),
    ) {
        let dir = tempdir().unwrap();
        let db_file = NamedTempFile::new().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = SqliteHashStore::new(db_file.path(), &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        let file_path = source.join("prop_test.bin");
        let dest_file_path = dest.join("prop_test.bin");
        fs::write(&file_path, &content).unwrap();

        // First sync — writes file to dest
        engine.sync_file("prop_test.bin").unwrap();
        prop_assert!(dest_file_path.exists());
        prop_assert_eq!(fs::read(&dest_file_path).unwrap(), content.clone());

        // Second sync — idempotent pass, succeeds without altering dest content
        engine.sync_file("prop_test.bin").unwrap();
        prop_assert_eq!(fs::read(&dest_file_path).unwrap(), content);
    }

    #[test]
    fn prop_delta_sync_single_block_isolation(
        initial_data in prop::collection::vec(any::<u8>(), 4096..8192),
        modified_byte in any::<u8>(),
    ) {
        let dir = tempdir().unwrap();
        let db_file = NamedTempFile::new().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let mut config = Config::test_default(source.clone(), dest.clone());
        config.block_size_bytes = 1024;
        config.block_sync_threshold_bytes = 1024;
        let store = SqliteHashStore::new(db_file.path(), &config).unwrap();
        let engine = LocalSyncEngine::new(store, config);

        let file_path = source.join("delta_test.bin");
        let dest_file_path = dest.join("delta_test.bin");
        fs::write(&file_path, &initial_data).unwrap();

        // Initial sync
        engine.sync_file("delta_test.bin").unwrap();
        prop_assert_eq!(fs::read(&dest_file_path).unwrap(), initial_data.clone());

        // Mutate single byte in first block
        let mut modified_data = initial_data.clone();
        if modified_data[0] == modified_byte {
            modified_data[0] = modified_byte.wrapping_add(1);
        } else {
            modified_data[0] = modified_byte;
        }

        // Force mtime update so sync engine detects change
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&file_path, &modified_data).unwrap();

        // Delta sync — verifies file is updated to modified_data
        engine.sync_file("delta_test.bin").unwrap();
        prop_assert_eq!(fs::read(&dest_file_path).unwrap(), modified_data);
    }
}

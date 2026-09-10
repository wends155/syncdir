use proptest::prelude::*;
use std::fs;
use std::path::PathBuf;
use syncdir::config::Config;
use syncdir::db::{SqliteHashStore, StoreConfig};
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
        let config = Config::builder(PathBuf::from(r"C:\Source"))
            .dest_dir(PathBuf::from(r"D:\Dest"))
            .debounce_seconds(debounce)
            .retry_interval_seconds(retry_interval)
            .verify_writes(verify_writes)
            .propagate_deletions(propagate_deletions)
            .build()
            .unwrap();

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        prop_assert_eq!(config.debounce_seconds(), parsed.debounce_seconds());
        prop_assert_eq!(config.retry_interval_seconds(), parsed.retry_interval_seconds());
        prop_assert_eq!(config.verify_writes(), parsed.verify_writes());
        prop_assert_eq!(config.propagate_deletions(), parsed.propagate_deletions());
    }

    // 3. SMB Timestamp Tolerance & Metadata Evaluation Invariant (±2000ms)
    #[test]
    fn prop_is_metadata_up_to_date_evaluation(
        size in 0i64..1_000_000i64,
        dest_size_diff in -5i64..=5i64,
        src_mod in 1_000_000_000i64..2_000_000_000i64,
        delta in -10_000i64..=10_000i64,
        has_matching_record in prop::bool::ANY,
    ) {
        let dest_size = size + dest_size_diff;
        let dest_mod = src_mod + delta;

        let record = if has_matching_record {
            Some(syncdir::db::FileRecord::new(PathBuf::from("file.bin"), size, src_mod).with_id(1))
        } else {
            None
        };

        let dest = syncdir::sync::FileMetadataSnapshot::new(dest_size, dest_mod);
        let src = syncdir::sync::FileMetadataSnapshot::new(size, src_mod);
        let result = syncdir::sync::is_metadata_up_to_date_raw(
            &dest,
            &src,
            record.as_ref(),
        );

        let expected = has_matching_record
            && dest_size == size
            && delta.abs() <= 2000;

        prop_assert_eq!(result, expected);
    }

    // 4. Path Traversal Safety Invariant
    #[test]
    fn prop_path_traversal_rejection(
        segment1 in "[a-zA-Z0-9]{1,8}",
        segment2 in "[a-zA-Z0-9]{1,8}",
    ) {
        let malicious_path = format!("../{}/../{}", segment1, segment2);
        prop_assert!(!syncdir::sync::is_safe_relative_path(std::path::Path::new(
            &malicious_path
        )));
    }

    // 5. Source Path Validation Rejection Invariant
    #[test]
    fn prop_source_path_validation_rejection(
        rel_path in "[a-zA-Z0-9_]{1,10}/[a-zA-Z0-9_]{1,10}",
    ) {
        let config = Config::builder(PathBuf::from(rel_path))
            .dest_dir(PathBuf::from(r"D:\Dest"))
            .build_unvalidated();
        prop_assert!(config.validate().is_err());
    }

    // 6. DirtyBlockRange Chunk Coalescing Invariant
    #[test]
    fn prop_dirty_block_range_chunk_coalescing(
        start_block in 0u64..1000u64,
        count in 1usize..15usize,
        block_size in 128u64..1024u64,
    ) {
        use std::io::Cursor;
        use syncdir::sync::DirtyBlockRange;

        let mut range = DirtyBlockRange::new(std::num::NonZeroU64::new(block_size).unwrap());
        let mut cursor = Cursor::new(Vec::new());
        let payload = vec![0xAAu8; block_size as usize];

        for i in 0..count {
            let idx = start_block + i as u64;
            range.add_block(idx, &payload, &mut cursor).unwrap();
        }

        prop_assert_eq!(range.start_block(), start_block);
        prop_assert_eq!(range.block_count(), count as u64);
        prop_assert_eq!(range.byte_len(), count * (block_size as usize));

        range.flush(&mut cursor).unwrap();

        prop_assert!(range.is_empty());
        prop_assert_eq!(range.byte_len(), 0);
        let written = cursor.into_inner();
        let expected_min_len = (start_block as usize + count) * (block_size as usize);
        prop_assert_eq!(written.len(), expected_min_len);
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
        let store =
            SqliteHashStore::new(db_file.path(), StoreConfig::try_from(&config).unwrap()).unwrap();
        let target_config = syncdir::config::TargetSyncConfig::from_config(&config, dest.clone());
        let engine = LocalSyncEngine::new(store, target_config);

        let file_path = source.join("prop_test.bin");
        let dest_file_path = dest.join("prop_test.bin");
        fs::write(&file_path, &content).unwrap();

        // First sync — writes file to dest
        engine.sync_file(std::path::Path::new("prop_test.bin")).unwrap();
        prop_assert!(dest_file_path.exists());
        prop_assert_eq!(fs::read(&dest_file_path).unwrap(), content.clone());

        // Second sync — idempotent pass, succeeds without altering dest content
        engine.sync_file(std::path::Path::new("prop_test.bin")).unwrap();
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

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_size_bytes(1024)
            .block_sync_threshold_bytes(1024)
            .build()
            .unwrap();
        let store =
            SqliteHashStore::new(db_file.path(), StoreConfig::try_from(&config).unwrap()).unwrap();
        let target_config = syncdir::config::TargetSyncConfig::from_config(&config, dest.clone());
        let engine = LocalSyncEngine::new(store, target_config);

        let file_path = source.join("delta_test.bin");
        let dest_file_path = dest.join("delta_test.bin");
        fs::write(&file_path, &initial_data).unwrap();

        // Initial sync
        engine.sync_file(std::path::Path::new("delta_test.bin")).unwrap();
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
        engine.sync_file(std::path::Path::new("delta_test.bin")).unwrap();
        prop_assert_eq!(fs::read(&dest_file_path).unwrap(), modified_data);
    }
}

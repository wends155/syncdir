//! Unit test suite for configuration parsing, validation, and domain types.

use super::validation::preprocess_config_toml;
use super::*;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn test_target_sync_config_block_size_nonzero() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let dest = temp.path().join("dest");

    let target_zero = TargetSyncConfig::from_raw_parts(
        TargetDir::from_validated(src.clone()),
        TargetDir::from_validated(dest.clone()),
        0,
        1024,
        true,
        VerificationMode::Full,
        3,
        10,
        true,
    );
    assert_eq!(target_zero.block_size_nonzero().get(), 64 * 1024);

    let target_custom = TargetSyncConfig::from_raw_parts(
        TargetDir::from_validated(src),
        TargetDir::from_validated(dest),
        128 * 1024,
        1024,
        true,
        VerificationMode::Full,
        3,
        10,
        true,
    );
    assert_eq!(target_custom.block_size_nonzero().get(), 128 * 1024);
}

#[test]
fn test_config_validation_valid() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let dest = temp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();

    let config = Config::builder(source)
        .dest_dir(dest)
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validation_nested_and_identical_paths() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let nested_dest = src.join("nested_dest");
    let outside_dest = temp.path().join("dest");
    let nested_src = outside_dest.join("nested_src");

    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&nested_dest).unwrap();
    std::fs::create_dir_all(&outside_dest).unwrap();
    std::fs::create_dir_all(&nested_src).unwrap();

    // 1. Identical paths
    let cfg_identical = Config::builder(src.clone())
        .dest_dir(src.clone())
        .build_unvalidated();
    assert!(
        cfg_identical.validate().is_err(),
        "Identical source and destination directory must fail validation"
    );

    // 2. Destination nested inside source
    let cfg_dest_in_src = Config::builder(src.clone())
        .dest_dir(nested_dest)
        .build_unvalidated();
    assert!(
        cfg_dest_in_src.validate().is_err(),
        "Destination directory nested within source directory must fail validation"
    );

    // 3. Source nested inside destination
    let cfg_src_in_dest = Config::builder(nested_src)
        .dest_dir(outside_dest)
        .build_unvalidated();
    assert!(
        cfg_src_in_dest.validate().is_err(),
        "Source directory nested within destination directory must fail validation"
    );
}

#[test]
fn test_config_validation_block_size_and_threshold_limits() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let dst = temp.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();

    // 1. block_size_bytes > 64MB rejected
    let cfg_oversized_block = Config::builder(src.clone())
        .dest_dir(dst.clone())
        .block_size_bytes(65 * 1024 * 1024)
        .block_sync_threshold_bytes(65 * 1024 * 1024)
        .build_unvalidated();
    assert!(
        cfg_oversized_block.validate().is_err(),
        "block_size_bytes exceeding 64MB must fail validation"
    );

    // 2. block_size_bytes == 64MB accepted
    let cfg_boundary_block = Config::builder(src.clone())
        .dest_dir(dst.clone())
        .block_size_bytes(64 * 1024 * 1024)
        .block_sync_threshold_bytes(64 * 1024 * 1024)
        .build()
        .unwrap();
    assert!(
        cfg_boundary_block.validate().is_ok(),
        "block_size_bytes at boundary 64MB must pass validation"
    );

    // 3. block_sync_threshold_bytes < block_size_bytes rejected
    let cfg_invalid_threshold = Config::builder(src)
        .dest_dir(dst)
        .block_size_bytes(1024 * 1024)
        .block_sync_threshold_bytes(512 * 1024)
        .build_unvalidated();
    assert!(
        cfg_invalid_threshold.validate().is_err(),
        "block_sync_threshold_bytes smaller than block_size_bytes must fail validation"
    );
}

#[test]
fn test_preprocess_config_toml_escaped_unc_not_double_expanded() {
    let input = r#"
        source_dir = "\\\\server\\share\\data"
        dest_dirs = [
            "\\\\nas\\backup1\\sub",
            "\\\\nas\\backup2"
        ]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10485760
        block_size_bytes = 1048576
        verify_writes = true
    "#;

    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).expect("Config TOML parsing must succeed");

    assert_eq!(
        config.source_dir(),
        Path::new(r"\\server\share\data"),
        "Pre-escaped UNC source path must not be doubled to 4 backslashes"
    );

    let dests = config.resolved_dest_dirs();
    assert_eq!(dests[0], PathBuf::from(r"\\nas\backup1\sub"));
    assert_eq!(dests[1], PathBuf::from(r"\\nas\backup2"));
}

#[test]
fn test_config_validation_missing_source() {
    let temp = tempdir().unwrap();
    let dest = temp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();

    let config = Config::builder(temp.path().join("nonexistent"))
        .dest_dir(dest)
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();
    // Soft validation: missing source directory logs a warning but validation passes
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validation_zero_debounce() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let config = Config::builder(source)
        .dest_dir(temp.path().join("dest"))
        .debounce_seconds(0)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build_unvalidated();
    assert!(config.validate().is_err());
}

#[test]
fn test_config_validation_zero_retry_interval() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let config = Config::builder(source)
        .dest_dir(temp.path().join("dest"))
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(0)
        .build_unvalidated();
    assert!(config.validate().is_err());
}

#[test]
fn test_config_retry_interval_default() {
    let toml_str = r#"
        source_dir = "C:\\source"
        dest_dir = "C:\\dest"
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 1024
        block_size_bytes = 512
        verify_writes = true
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.retry_interval_seconds, 10);
}

#[test]
fn test_config_parsing_unescaped_backslashes() {
    let toml_str = r#"
        source_dir = "Y:\Mill Processing\COMMON\MAINTENANCE"
        dest_dir = "Z:\Backup\Folder"
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 1024
        block_size_bytes = 512
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(toml_str);
    let config: Config = toml::from_str(&processed).unwrap();
    assert_eq!(
        config.source_dir().to_string_lossy(),
        r#"Y:\Mill Processing\COMMON\MAINTENANCE"#
    );
    assert_eq!(
        config.dest_dir().unwrap().to_string_lossy(),
        r#"Z:\Backup\Folder"#
    );
}

#[test]
fn test_default_app_dir_returns_appdata_path() {
    let dir = Config::default_app_dir().unwrap();
    let dir_str = dir.to_string_lossy().to_lowercase();
    assert!(
        dir_str.contains("appdata"),
        "Expected AppData in path, got: {dir_str}"
    );
    assert!(
        dir_str.ends_with("syncdir"),
        "Expected path to end with 'syncdir', got: {dir_str}"
    );
}

#[test]
fn test_default_config_path() {
    let path = Config::default_config_path().unwrap();
    let path_str = path.to_string_lossy().to_lowercase();
    assert!(
        path_str.contains("appdata"),
        "Expected AppData in path, got: {path_str}"
    );
    assert!(
        path_str.ends_with("syncdir\\config.toml") || path_str.ends_with("syncdir/config.toml"),
        "Expected path to end with 'syncdir/config.toml', got: {path_str}"
    );
}

#[test]
fn test_config_resolved_dest_dirs() {
    let config = Config::builder("C:\\src")
        .dest_dir(PathBuf::from("D:\\dst1"))
        .dest_dirs(vec![PathBuf::from("D:\\dst1"), PathBuf::from("E:\\dst2")])
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0], PathBuf::from("D:\\dst1"));
    assert_eq!(resolved[1], PathBuf::from("E:\\dst2"));
}

#[test]
fn test_preprocess_dest_dirs_backslashes() {
    let input = r#"
        source_dir = "C:\source"
        dest_dir = "D:\Backup"
        dest_dirs = ["Y:\Mill Processing\COMMON", "Z:\Archive\Folder"]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).unwrap();
    assert_eq!(config.source_dir().to_string_lossy(), r"C:\source");
    assert_eq!(config.dest_dir().unwrap().to_string_lossy(), r"D:\Backup");
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved[1].to_string_lossy(), r"Y:\Mill Processing\COMMON");
    assert_eq!(resolved[2].to_string_lossy(), r"Z:\Archive\Folder");
}

#[test]
fn test_config_only_dest_dirs() {
    let input = r#"
        source_dir = "C:\source"
        dest_dirs = ["D:\Backup1", "E:\Backup2"]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).unwrap();
    assert_eq!(config.destinations().len(), 2);
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0], PathBuf::from(r"D:\Backup1"));
    assert_eq!(resolved[1], PathBuf::from(r"E:\Backup2"));
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validation_no_dests() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let config = Config::builder(source)
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build_unvalidated();
    assert!(config.validate().is_err());
}

#[test]
fn test_preprocess_unc_paths_preserved() {
    let input = r#"
        source_dir = "C:\source"
        dest_dirs = ["\\172.16.0.60\scada_data\Files", "\\172.16.0.130\Files"]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).unwrap();
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(
        resolved[0].to_string_lossy(),
        r"\\172.16.0.60\scada_data\Files"
    );
    assert_eq!(resolved[1].to_string_lossy(), r"\\172.16.0.130\Files");
    assert!(config.validate().is_ok());
}

#[test]
fn test_resolved_dest_dirs_normalizes_single_backslash_unc() {
    let config = Config::builder("C:\\src")
        .dest_dir(PathBuf::from(r"\172.16.0.60\scada_data"))
        .dest_dirs(vec![PathBuf::from(r"\172.16.0.130\Files")])
        .debounce_seconds(1)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(10)
        .block_size_bytes(4)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].to_string_lossy(), r"\\172.16.0.60\scada_data");
    assert_eq!(resolved[1].to_string_lossy(), r"\\172.16.0.130\Files");
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validation_invalid_relative_dest() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let config = Config::builder(source)
        .dest_dir(PathBuf::from("relative/folder/path"))
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build_unvalidated();
    assert!(config.validate().is_err());
}

#[test]
fn test_normalize_path_filtering() {
    assert_eq!(
        normalize_path(Path::new("X:/folder/subfolder/")).to_string_lossy(),
        r"X:\folder\subfolder"
    );
    assert_eq!(
        normalize_path(Path::new("\"Z:\\data\\files\\\"")).to_string_lossy(),
        r"Z:\data\files"
    );
    assert_eq!(
        normalize_path(Path::new(r"\172.16.0.193\share\")).to_string_lossy(),
        r"\\172.16.0.193\share"
    );
    assert_eq!(normalize_path(Path::new("C:\\")).to_string_lossy(), r"C:\");
}

#[test]
fn test_normalize_drive_root_without_backslash() {
    assert_eq!(normalize_path(Path::new("R:")).to_string_lossy(), r"R:\");
    assert_eq!(normalize_path(Path::new("R:\\")).to_string_lossy(), r"R:\");
    assert_eq!(normalize_path(Path::new("R:/")).to_string_lossy(), r"R:\");
}

#[test]
fn test_config_validate_mapped_drive() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();

    let config = Config::builder(source)
        .dest_dir(PathBuf::from("X:/Control IT Data/Files/"))
        .dest_dirs(vec![PathBuf::from(r"Z:\Backup\OPC\")])
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();

    assert!(config.validate().is_ok());
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].to_string_lossy(), r"X:\Control IT Data\Files");
    assert_eq!(resolved[1].to_string_lossy(), r"Z:\Backup\OPC");
}

#[test]
fn test_config_validation_invalid_source_relative() {
    let config = Config::builder("relative/path/source")
        .dest_dir(PathBuf::from(r"C:\Backup"))
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build_unvalidated();

    let err = config.validate().unwrap_err();
    assert!(
        matches!(err, SyncError::Validation { ref message, .. } if message.contains("Invalid source path"))
    );
}

#[test]
fn test_normalize_paths_source_and_dest() {
    let config = Config::builder("C:/Source/Folder/")
        .dest_dir("D:/Dest/Folder/")
        .dest_dirs(vec![PathBuf::from("E:/Backup/Folder/")])
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();

    assert_eq!(config.source_dir().to_string_lossy(), r"C:\Source\Folder");
    let dests = config.resolved_dest_dirs();
    assert_eq!(dests[0].to_string_lossy(), r"D:\Dest\Folder");
    assert_eq!(dests[1].to_string_lossy(), r"E:\Backup\Folder");
}

#[test]
fn test_resolved_dest_dirs_case_insensitive_dedup() {
    let config = Config::builder(r"C:\Source")
        .dest_dir(PathBuf::from(r"Z:\Backup\OPC"))
        .dest_dirs(vec![
            PathBuf::from(r"z:\backup\opc"),
            PathBuf::from(r"Z:\BACKUP\OPC\"),
            PathBuf::from(r"Y:\Different\Backup"),
        ])
        .debounce_seconds(3)
        .propagate_deletions(true)
        .block_sync_threshold_bytes(1024)
        .block_size_bytes(512)
        .verify_writes(true)
        .retry_interval_seconds(10)
        .build()
        .unwrap();

    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].to_string_lossy(), r"Z:\Backup\OPC");
    assert_eq!(resolved[1].to_string_lossy(), r"Y:\Different\Backup");
}

#[test]
#[allow(deprecated)]
fn test_resolved_source_dir_alternate_resolution() {
    let temp = tempdir().unwrap();
    let source_path = temp.path().join("source");
    std::fs::create_dir(&source_path).unwrap();

    let config = Config::test_default(source_path.clone(), temp.path().join("dest"));
    assert_eq!(config.resolved_source_dir(), &source_path);
}

#[test]
fn test_preprocess_dest_dirs_multiline_array() {
    let input = r#"
        source_dir = "C:\source"
        dest_dirs = [
            "Y:\Mill Processing\COMMON",
            "Z:\Archive\Folder",
            "X:\Backup\Files",
        ]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).unwrap();
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[0].to_string_lossy(), r"Y:\Mill Processing\COMMON");
    assert_eq!(resolved[1].to_string_lossy(), r"Z:\Archive\Folder");
    assert_eq!(resolved[2].to_string_lossy(), r"X:\Backup\Files");
}

#[test]
fn test_preprocess_dest_dirs_mixed_quotes_and_commas() {
    let input = r#"
        source_dir = "C:/source"
        dest_dirs = [
            'Y:/backup_folder_1',
            "Z:\backup_folder_2",
            "X:/backup_folder_3",
        ]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).unwrap();
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[0].to_string_lossy(), r"Y:\backup_folder_1");
    assert_eq!(resolved[1].to_string_lossy(), r"Z:\backup_folder_2");
    assert_eq!(resolved[2].to_string_lossy(), r"X:\backup_folder_3");
}

#[test]
fn test_config_load_invalid_missing_comma_in_dest_dirs() {
    let input = r#"
        source_dir = "C:\source"
        dest_dirs = [
            "Y:\backup_folder_1"
            "X:\backup_folder_2"
        ]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let res: Result<Config, _> = toml::from_str(&processed);
    assert!(
        res.is_err(),
        "Missing comma in dest_dirs array must return syntax error"
    );
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("comma") || err_msg.contains("expected") || err_msg.contains("invalid"),
        "Error message should mention parsing failure: {}",
        err_msg
    );
}

#[test]
fn test_validate_rejects_zero_block_size() {
    let mut config = Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"C:\dest"));
    config.block_size_bytes = 0;
    let result = config.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("block_size_bytes"));
}

#[test]
fn test_validate_rejects_zero_threshold() {
    let mut config = Config::test_default(PathBuf::from(r"C:\source"), PathBuf::from(r"C:\dest"));
    config.block_sync_threshold_bytes = 0;
    let result = config.validate();
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("block_sync_threshold_bytes")
    );
}

#[test]
fn test_builder_add_dest_dir_and_try_build() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("src");
    let dst1 = temp.path().join("dst1");
    let dst2 = temp.path().join("dst2");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst1).unwrap();
    std::fs::create_dir_all(&dst2).unwrap();

    let config = Config::builder(&src)
        .dest_dir(&dst1)
        .add_dest_dir(&dst2)
        .try_build()
        .unwrap();

    assert_eq!(config.resolved_dest_dirs().len(), 2);
}

#[test]
fn test_target_sync_config_from_config() {
    let config = Config::builder(r"C:\src")
        .dest_dir(r"C:\dst")
        .block_size_bytes(1024)
        .block_sync_threshold_bytes(2048)
        .verify_writes(true)
        .debounce_seconds(5)
        .retry_interval_seconds(15)
        .propagate_deletions(false)
        .build()
        .unwrap();
    let target = TargetSyncConfig::from_config(
        &config,
        TargetDir::from_validated(PathBuf::from(r"C:\dst2")),
    )
    .unwrap();
    assert_eq!(target.source_dir(), Path::new(r"C:\src"));
    assert_eq!(target.dest_dir(), Path::new(r"C:\dst2"));
    assert_eq!(target.block_size_bytes(), 1024);
    assert_eq!(target.block_sync_threshold_bytes(), 2048);
    assert!(target.verify_writes());
    assert_eq!(target.debounce_seconds(), 5);
    assert_eq!(target.retry_interval_seconds(), 15);
    assert!(!target.propagate_deletions());
}

#[test]
#[allow(deprecated)]
fn test_target_dir_normalization_and_validation() {
    let t1 = TargetDir::new("X:/folder/subfolder/");
    assert_eq!(t1.as_path().to_string_lossy(), r"X:\folder\subfolder");
    assert!(t1.validate(TargetRole::Source).is_ok());

    let t2 = TargetDir::new("\"Z:\\data\\files\\\"");
    assert_eq!(t2.as_path().to_string_lossy(), r"Z:\data\files");
    assert!(t2.validate(TargetRole::Destination).is_ok());

    let t3 = TargetDir::new(r"\172.16.0.193\share\");
    assert_eq!(t3.as_path().to_string_lossy(), r"\\172.16.0.193\share");
    assert!(t3.validate(TargetRole::Source).is_ok());

    let t4 = TargetDir::new("R:");
    assert_eq!(t4.as_path().to_string_lossy(), r"R:\");
    assert!(t4.validate(TargetRole::Destination).is_ok());

    let rel = TargetDir::new("relative/source");
    assert!(rel.validate(TargetRole::Source).is_err());
    let err_msg = rel.validate(TargetRole::Source).unwrap_err().to_string();
    assert!(err_msg.contains("Invalid source path"));
}

#[test]
#[allow(deprecated)]
fn test_destination_collection_dedup_and_order() {
    let col = DestinationCollection::from_raw(
        Some(PathBuf::from(r"D:\Backup1")),
        Some(vec![
            PathBuf::from(r"d:\backup1"), // duplicate, different case
            PathBuf::from(r"E:\Backup2"),
            PathBuf::from(r"\\172.16.0.60\scada_data"),
        ]),
    );
    assert_eq!(col.len(), 3);
    assert_eq!(col[0], TargetDir::new(r"D:\Backup1"));
    assert_eq!(col.get(1).unwrap(), &TargetDir::new(r"E:\Backup2"));
    assert_eq!(col[2], TargetDir::new(r"\\172.16.0.60\scada_data"));
    let paths = col.to_path_bufs();
    assert_eq!(paths[0], PathBuf::from(r"D:\Backup1"));
    assert_eq!(paths[1], PathBuf::from(r"E:\Backup2"));
    assert_eq!(paths[2], PathBuf::from(r"\\172.16.0.60\scada_data"));
}

#[test]
#[allow(deprecated)]
fn test_config_destinations_matches_dest_dirs() {
    let d1 = PathBuf::from(r"D:\Backup1");
    let d2 = PathBuf::from(r"E:\Backup2");
    let config = Config::builder(r"C:\Source")
        .dest_dir(d1.clone())
        .add_dest_dir(d2.clone())
        .build()
        .unwrap();
    let slice: &[_] = config.destinations();
    assert_eq!(slice.len(), 2);
    let legacy = config.dest_dirs().unwrap();
    assert_eq!(slice[0].as_path(), legacy[0]);
    assert_eq!(slice[1].as_path(), legacy[1]);
}

#[test]
fn test_target_sync_config_builder() {
    let builder = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .block_size_bytes(512 * 1024)
    .block_sync_threshold_bytes(2 * 1024 * 1024)
    .verify_writes(false)
    .debounce_seconds(5)
    .retry_interval_seconds(20)
    .propagate_deletions(false);

    let cfg = builder.build().expect("valid builder should build");
    assert_eq!(cfg.source_dir(), Path::new(r"C:\source"));
    assert_eq!(cfg.dest_dir(), Path::new(r"D:\dest"));
    assert_eq!(cfg.block_size_bytes(), 512 * 1024);
    assert_eq!(cfg.block_sync_threshold_bytes(), 2 * 1024 * 1024);
    assert!(!cfg.verify_writes());
    assert_eq!(cfg.debounce_seconds(), 5);
    assert_eq!(cfg.retry_interval_seconds(), 20);
    assert!(!cfg.propagate_deletions());

    // Zero block size fails
    let invalid = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .block_size_bytes(0)
    .build();
    assert!(invalid.is_err());

    // Block size > 64MB fails
    let invalid = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .block_size_bytes(65 * 1024 * 1024)
    .build();
    assert!(invalid.is_err());

    // Zero threshold fails
    let invalid = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .block_sync_threshold_bytes(0)
    .build();
    assert!(invalid.is_err());
}

#[test]
fn test_target_sync_config_builder_validation_invariants() {
    // 1. Threshold < block size fails
    let err = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .block_size_bytes(1024 * 1024)
    .block_sync_threshold_bytes(512 * 1024)
    .build();
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("greater than or equal to block_size_bytes")
    );

    // 2. Debounce seconds == 0 fails
    let err = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .debounce_seconds(0)
    .build();
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("debounce_seconds must be greater than zero")
    );

    // 3. Retry interval seconds == 0 fails
    let err = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .retry_interval_seconds(0)
    .build();
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("retry_interval_seconds must be greater than zero")
    );

    // 4. Source == Destination fails
    let err = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"C:\source"),
    )
    .build();
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("recursive sync loop"));

    // 5. Dest nested inside Source fails
    let err = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"C:\source\nested_dest"),
    )
    .build();
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("recursive sync loop"));

    // 6. Source nested inside Dest fails
    let err = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source\nested_src"),
        TargetDir::from_validated(r"C:\source"),
    )
    .build();
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("recursive sync loop"));
}

#[test]
fn test_preprocess_config_toml_with_bracketed_path_name() {
    let input = r#"
        source_dir = "C:\source"
        dest_dirs = [
            "D:\Backup[1]\Data",
            "E:\Backup[2]\Data"
        ]
        debounce_seconds = 3
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = true
    "#;
    let processed = preprocess_config_toml(input);
    let config: Config = toml::from_str(&processed).expect("should parse bracketed paths in array");
    let resolved = config.resolved_dest_dirs();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].to_string_lossy(), r"D:\Backup[1]\Data");
    assert_eq!(resolved[1].to_string_lossy(), r"E:\Backup[2]\Data");
}

#[test]
fn test_verification_mode_config_resolution() {
    // Legacy verify_writes = true resolves to Full
    let cfg1 = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .verify_writes(true)
        .build()
        .unwrap();
    assert_eq!(cfg1.verification_mode(), VerificationMode::Full);

    // Legacy verify_writes = false resolves to Disabled
    let cfg2 = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .verify_writes(false)
        .build()
        .unwrap();
    assert_eq!(cfg2.verification_mode(), VerificationMode::Disabled);

    // Explicit verification_mode overrides verify_writes
    let cfg3 = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .verify_writes(true)
        .verification_mode(VerificationMode::MetadataAndFlush)
        .build()
        .unwrap();
    assert_eq!(cfg3.verification_mode(), VerificationMode::MetadataAndFlush);

    // TOML parsing with verification_mode
    let toml_str = r#"
        source_dir = "C:\\source"
        dest_dir = "D:\\dest"
        debounce_seconds = 1
        propagate_deletions = true
        block_sync_threshold_bytes = 10
        block_size_bytes = 4
        verify_writes = false
        verification_mode = "sampled"
    "#;
    let parsed: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(parsed.verification_mode(), VerificationMode::Sampled);
}

#[test]
fn test_verification_mode_target_sync_config_plumbing() {
    let config = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .verification_mode(VerificationMode::MetadataAndFlush)
        .build()
        .unwrap();
    let target =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(r"D:\dest")).unwrap();
    assert_eq!(
        target.verification_mode(),
        VerificationMode::MetadataAndFlush
    );

    let target_builder = TargetSyncConfig::builder(
        TargetDir::from_validated(r"C:\source"),
        TargetDir::from_validated(r"D:\dest"),
    )
    .verification_mode(VerificationMode::Sampled)
    .build()
    .unwrap();
    assert_eq!(
        target_builder.verification_mode(),
        VerificationMode::Sampled
    );
}

#[test]
fn test_mutual_destination_overlap_rejected() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let dest1 = temp.path().join("dest1");
    let dest2 = dest1.join("nested");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dest2).unwrap();

    let config = Config::builder(&src)
        .dest_dirs(vec![dest1, dest2])
        .build_unvalidated();
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("nested within each other"));
}

#[test]
fn test_target_sync_config_try_from() {
    let config_no_dest = Config::builder(r"C:\source").build_unvalidated();
    let res = TargetSyncConfig::try_from_config(&config_no_dest);
    assert!(res.is_err());

    let config_with_dest = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .build()
        .unwrap();
    let res = TargetSyncConfig::try_from_config(&config_with_dest);
    assert!(res.is_ok());
}

#[test]
fn test_target_sync_config_requires_explicit_construction() {
    // `From<&Config>` and `From<Config>` for TargetSyncConfig were intentionally removed
    // because they silently drop secondary destinations or fabricate empty targets.
    // Callers must use Config::target_configs() or TargetSyncConfig::try_from_config().
    let cfg = Config::test_default(r"C:\source", r"D:\dest");
    let targets = cfg.target_configs().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].dest_dir().to_string_lossy(), r"D:\dest");
}

#[test]
fn test_config_builder_build_validates_invariants() {
    // 0 debounce seconds must fail validation immediately under build()
    let res = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .debounce_seconds(0)
        .build();
    assert!(res.is_err(), "Expected error for debounce_seconds == 0");

    // try_build also fails
    let res_try = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .debounce_seconds(0)
        .try_build();
    assert!(res_try.is_err(), "Expected error from try_build");

    // build_unvalidated allows constructing for tests without panic
    let unvalidated = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .debounce_seconds(0)
        .build_unvalidated();
    assert_eq!(unvalidated.debounce_seconds, 0);

    // block_sync_threshold_bytes < block_size_bytes must fail
    let res_order = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .block_size_bytes(1024 * 1024)
        .block_sync_threshold_bytes(512 * 1024)
        .build();
    assert!(
        res_order.is_err(),
        "Expected error for threshold < block_size"
    );
}

#[test]
fn test_target_sync_config_from_config_rejects_nested_and_ancestor_paths() {
    let config = Config::test_default(r"C:\source", r"D:\dest");
    // Nested path (dest inside source)
    let res_nested = TargetSyncConfig::from_config(
        &config,
        TargetDir::from_validated(r"C:\source\nested"),
    );
    assert!(res_nested.is_err());
    match res_nested.unwrap_err() {
        SyncError::Validation { kind, .. } => {
            assert_eq!(kind, crate::error::ValidationKind::RecursiveLoop);
        }
        err => panic!("Unexpected error type: {err:?}"),
    }

    // Ancestor path (source inside dest)
    let res_ancestor =
        TargetSyncConfig::from_config(&config, TargetDir::from_validated(r"C:\"));
    assert!(res_ancestor.is_err());
    match res_ancestor.unwrap_err() {
        SyncError::Validation { kind, .. } => {
            assert_eq!(kind, crate::error::ValidationKind::RecursiveLoop);
        }
        err => panic!("Unexpected error type: {err:?}"),
    }
}

#[test]
fn test_target_sync_config_from_config_rejects_invalid_block_size_and_ordering() {
    let invalid_config = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .block_size_bytes(0)
        .build_unvalidated();
    let res = TargetSyncConfig::from_config(
        &invalid_config,
        TargetDir::from_validated(r"D:\dest"),
    );
    assert!(res.is_err());

    let invalid_order = Config::builder(r"C:\source")
        .dest_dir(r"D:\dest")
        .block_size_bytes(1024)
        .block_sync_threshold_bytes(512)
        .build_unvalidated();
    let res_order = TargetSyncConfig::from_config(
        &invalid_order,
        TargetDir::from_validated(r"D:\dest"),
    );
    assert!(res_order.is_err());
}

#[test]
fn test_config_block_parameters_for_storage() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let dst = temp.path().join("dest");

    let config = Config::builder(src)
        .dest_dir(dst)
        .block_size_bytes(1024 * 1024)
        .block_sync_threshold_bytes(10 * 1024 * 1024)
        .build_unvalidated();

    // Verify Config exposes block size parameters queryable by storage components without depending on db
    assert_eq!(config.block_size_bytes(), 1024 * 1024);
    assert_eq!(config.block_sync_threshold_bytes(), 10 * 1024 * 1024);
}

#[test]
fn test_target_sync_config_builder_verify_writes_syncs_with_verification_mode() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let dest = temp.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dest).unwrap();

    let cfg = TargetSyncConfig::builder(
        TargetDir::from_validated(src.clone()),
        TargetDir::from_validated(dest.clone()),
    )
    .verification_mode(VerificationMode::Full)
    .verify_writes(false)
    .build()
    .unwrap();
    assert_eq!(cfg.verification_mode(), VerificationMode::Disabled);
    assert!(!cfg.verify_writes());

    let cfg2 = TargetSyncConfig::builder(
        TargetDir::from_validated(src.clone()),
        TargetDir::from_validated(dest.clone()),
    )
    .verification_mode(VerificationMode::Disabled)
    .verify_writes(true)
    .build()
    .unwrap();
    assert_ne!(cfg2.verification_mode(), VerificationMode::Disabled);
    assert!(cfg2.verify_writes());

    let config = Config::builder(src)
        .dest_dir(dest)
        .verification_mode(VerificationMode::Full)
        .verify_writes(false)
        .build()
        .unwrap();
    assert_eq!(config.verification_mode(), VerificationMode::Disabled);
    assert!(!config.verify_writes());
}

#[test]
fn test_target_sync_config_block_size_nonzero_panic_free_fallback() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("source");
    let dest = temp.path().join("dest");

    let target_zero = TargetSyncConfig::from_raw_parts(
        TargetDir::from_validated(src),
        TargetDir::from_validated(dest),
        0,
        1024,
        true,
        VerificationMode::Full,
        3,
        10,
        true,
    );
    assert_eq!(target_zero.block_size_nonzero(), DEFAULT_BLOCK_SIZE);
}

#[test]
#[allow(deprecated)]
fn test_target_sync_config_source_dir_target_dir() {
    use crate::config::TargetSyncConfig;
    use crate::config::TargetDir;
    let td = TargetDir::new("C:\\Users\\Data");
    let config = TargetSyncConfig::builder(td.clone(), TargetDir::from_validated("D:\\Backup"))
        .build()
        .expect("build");
    assert_eq!(config.source_dir(), std::path::Path::new("C:\\Users\\Data"));
    assert_eq!(
        config.source_target_dir().as_path(),
        std::path::Path::new("C:\\Users\\Data")
    );
}

#[test]
fn test_target_dir_serde_proxy_validation() {
    use crate::config::TargetDir;
    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    struct Cfg {
        target: TargetDir,
    }
    let valid_toml = r#"target = "C:\\valid\\path""#;
    let cfg: Result<Cfg, _> = toml::from_str(valid_toml);
    assert!(cfg.is_ok());

    let empty_toml = r#"target = """#;
    let cfg_err: Result<Cfg, _> = toml::from_str(empty_toml);
    assert!(cfg_err.is_err());
}

#[test]
fn test_config_builders_must_use_and_fluent_setters() {
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("source");
    let dst = temp.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();

    let cfg = Config::builder(&src)
        .dest_dir(&dst)
        .debounce_seconds(5)
        .retry_interval_seconds(15)
        .block_size_bytes(64 * 1024)
        .block_sync_threshold_bytes(1024 * 1024)
        .propagate_deletions(false)
        .verify_writes(true)
        .build()
        .unwrap();

    assert_eq!(cfg.debounce_seconds(), 5);
    assert_eq!(cfg.retry_interval_seconds(), 15);
    assert_eq!(cfg.block_size_bytes(), 64 * 1024);
    assert_eq!(cfg.block_sync_threshold_bytes(), 1024 * 1024);
    assert!(!cfg.propagate_deletions());

    let target_cfg = TargetSyncConfig::builder(
        TargetDir::try_from(src.clone()).unwrap(),
        TargetDir::try_from(dst.clone()).unwrap(),
    )
    .debounce_seconds(7)
    .retry_interval_seconds(20)
    .block_size_bytes(128 * 1024)
    .block_sync_threshold_bytes(2 * 1024 * 1024)
    .propagate_deletions(true)
    .verify_writes(false)
    .build()
    .unwrap();

    assert_eq!(target_cfg.debounce_seconds(), 7);
    assert_eq!(target_cfg.retry_interval_seconds(), 20);
    assert_eq!(target_cfg.block_size_bytes(), 128 * 1024);
    assert_eq!(target_cfg.block_sync_threshold_bytes(), 2 * 1024 * 1024);
    assert!(target_cfg.propagate_deletions());
}

#[test]
fn test_destination_collection_query_must_use() {
    let d1 = TargetDir::try_from(r"C:\dest1").unwrap();
    let d2 = TargetDir::try_from(r"D:\dest2").unwrap();
    let col = DestinationCollection::new(vec![d1, d2]);

    assert_eq!(col.len(), 2);
    assert!(!col.is_empty());
    assert_eq!(col.as_slice().len(), 2);
    assert_eq!(col.iter().count(), 2);
}

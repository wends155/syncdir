#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

use build_script::{validate_icon_bytes, BuildResourceError};

#[test]
fn test_validate_icon_bytes_valid() {
    let valid_ico: [u8; 6] = [0x00, 0x00, 0x01, 0x00, 0x01, 0x00];
    assert_eq!(validate_icon_bytes(&valid_ico), Ok(()));
}

#[test]
fn test_validate_icon_bytes_too_short() {
    let short: [u8; 4] = [0x00, 0x00, 0x01, 0x00];
    assert!(matches!(
        validate_icon_bytes(&short),
        Err(BuildResourceError::IconInvalid(_))
    ));
}

#[test]
fn test_validate_icon_bytes_invalid_reserved() {
    let bad_reserved: [u8; 6] = [0x01, 0x00, 0x01, 0x00, 0x01, 0x00];
    assert!(matches!(
        validate_icon_bytes(&bad_reserved),
        Err(BuildResourceError::IconInvalid(_))
    ));
}

#[test]
fn test_validate_icon_bytes_invalid_type() {
    let bad_type: [u8; 6] = [0x00, 0x00, 0x02, 0x00, 0x01, 0x00]; // 2 = CUR
    assert!(matches!(
        validate_icon_bytes(&bad_type),
        Err(BuildResourceError::IconInvalid(_))
    ));
}

#[test]
fn test_validate_icon_bytes_zero_images() {
    let zero_images: [u8; 6] = [0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
    assert!(matches!(
        validate_icon_bytes(&zero_images),
        Err(BuildResourceError::IconInvalid(_))
    ));
}

use build_script::validate_icon_asset;
use std::io::Write;

#[test]
fn test_validate_icon_asset_not_found() {
    let non_existent = std::path::PathBuf::from("non_existent_path_12345.ico");
    assert_eq!(
        validate_icon_asset(&non_existent),
        Err(BuildResourceError::IconNotFound(non_existent))
    );
}

#[test]
fn test_validate_icon_asset_valid_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test_icon.ico");
    let mut file = std::fs::File::create(&file_path).unwrap();
    file.write_all(&[0x00, 0x00, 0x01, 0x00, 0x01, 0x00]).unwrap();
    assert_eq!(validate_icon_asset(&file_path), Ok(()));
}

#[test]
fn test_validate_icon_asset_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("large_icon.ico");
    let mut file = std::fs::File::create(&file_path).unwrap();
    let large_buf = vec![0u8; 524_289]; // 512KB + 1 byte
    file.write_all(&large_buf).unwrap();
    assert!(matches!(
        validate_icon_asset(&file_path),
        Err(BuildResourceError::IconInvalid(_))
    ));
}

use build_script::{validate_path_safety, ResourceBuildConfig};

#[test]
fn test_validate_path_safety_valid() {
    assert_eq!(
        validate_path_safety(std::path::Path::new("C:\\Valid\\Path")),
        Ok(())
    );
}

#[test]
fn test_validate_path_safety_null_bytes() {
    assert!(matches!(
        validate_path_safety(std::path::Path::new("C:\\Invalid\0Path")),
        Err(BuildResourceError::UnsafePath(_))
    ));
}

#[test]
fn test_config_from_env_defaults() {
    let empty_lookup = |_var: &str| None;
    let config = ResourceBuildConfig::from_env_with(empty_lookup);
    assert_eq!(config.winres_toolkit_path, None);
    assert_eq!(config.rc_path, None);
    assert_eq!(config.windows_sdk_path, None);
    assert!(!config.allow_missing_icon());
}

#[test]
fn test_config_from_env_truthy_flags() {
    for flag in &["1", "true", "TRUE", "yes", "YES"] {
        let lookup = |var: &str| {
            if var == "SYNCDIR_ALLOW_MISSING_ICON" {
                Some((*flag).to_string())
            } else {
                None
            }
        };
        let config = ResourceBuildConfig::from_env_with(lookup);
        assert!(config.allow_missing_icon());
    }
}

#[test]
fn test_resolve_toolkit_tier1_precedence() {
    let mut config = ResourceBuildConfig::from_env_with(|_| None);
    config.winres_toolkit_path = Some(std::path::PathBuf::from("C:\\ToolkitOverride"));
    config.rc_path = Some(std::path::PathBuf::from("C:\\RcOverride\\rc.exe"));
    let checker = |p: &std::path::Path| p == std::path::Path::new("C:\\ToolkitOverride");
    assert_eq!(
        config.resolve_toolkit_dir_with(checker),
        Ok(Some(std::path::PathBuf::from("C:\\ToolkitOverride")))
    );
}

#[test]
fn test_resolve_toolkit_tier2_rc_path_file() {
    let mut config = ResourceBuildConfig::from_env_with(|_| None);
    config.rc_path = Some(std::path::PathBuf::from("C:\\Tools\\bin\\rc.exe"));
    let checker = |p: &std::path::Path| p == std::path::Path::new("C:\\Tools\\bin\\rc.exe");
    assert_eq!(
        config.resolve_toolkit_dir_with(checker),
        Ok(Some(std::path::PathBuf::from("C:\\Tools\\bin")))
    );
}

#[test]
fn test_resolve_toolkit_tier2_rc_path_dir() {
    let mut config = ResourceBuildConfig::from_env_with(|_| None);
    config.rc_path = Some(std::path::PathBuf::from("C:\\Tools\\bin"));
    let checker = |p: &std::path::Path| {
        p == std::path::Path::new("C:\\Tools\\bin\\rc.exe")
            || p == std::path::Path::new("C:\\Tools\\bin\\windres.exe")
    };
    assert_eq!(
        config.resolve_toolkit_dir_with(checker),
        Ok(Some(std::path::PathBuf::from("C:\\Tools\\bin")))
    );
}

#[test]
fn test_resolve_toolkit_tier3_sdk_root() {
    let mut config = ResourceBuildConfig::from_env_with(|_| None);
    config.windows_sdk_path =
        Some(std::path::PathBuf::from("C:\\Program Files (x86)\\Windows Kits\\10"));
    let expected_bin =
        std::path::PathBuf::from("C:\\Program Files (x86)\\Windows Kits\\10\\bin\\x64");
    let checker = move |p: &std::path::Path| {
        p == std::path::Path::new(
            "C:\\Program Files (x86)\\Windows Kits\\10\\bin\\x64\\rc.exe",
        )
    };
    assert_eq!(
        config.resolve_toolkit_dir_with(checker),
        Ok(Some(expected_bin))
    );
}

#[test]
fn test_resolve_toolkit_tier4_fallback() {
    let config = ResourceBuildConfig::from_env_with(|_| None);
    let checker = |_p: &std::path::Path| false;
    assert_eq!(config.resolve_toolkit_dir_with(checker), Ok(None));
}

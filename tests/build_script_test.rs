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

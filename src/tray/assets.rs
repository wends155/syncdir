//! Compile-time system tray icon asset generation and static RGBA buffers.

use super::EngineStatus;
use crate::error::SyncError;
use std::sync::OnceLock;
use tray_icon::Icon;

/// Icon pixel dimensions (32x32).
pub const ICON_SIZE: u32 = 32;

/// Total byte size of a 32x32 RGBA icon buffer (4,096 bytes).
pub const ICON_BUFFER_LEN: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;

/// Generate a status-specific 32x32 RGBA pixel buffer entirely at compile time.
#[must_use]
pub const fn generate_status_rgba(status: EngineStatus) -> [u8; ICON_BUFFER_LEN] {
    let mut rgba = [0u8; ICON_BUFFER_LEN];
    let (border_r, border_g, border_b) = match status {
        EngineStatus::Healthy => (66, 133, 244),
        EngineStatus::Degraded => (255, 140, 0),
        EngineStatus::SourceOffline => (219, 68, 85),
        EngineStatus::DestinationOffline => (244, 180, 0),
        EngineStatus::BothOffline => (180, 180, 180),
    };
    let (center_r, center_g, center_b) = match status {
        EngineStatus::Healthy => (255, 255, 255),
        EngineStatus::Degraded
        | EngineStatus::SourceOffline
        | EngineStatus::DestinationOffline
        | EngineStatus::BothOffline => (80, 80, 80),
    };

    let mut y = 0;
    while y < 32 {
        let mut x = 0;
        while x < 32 {
            let idx = (y * 32 + x) * 4;
            let is_border = x < 4 || x >= 28 || y < 4 || y >= 28;
            let (r, g, b) = if is_border {
                (border_r, border_g, border_b)
            } else {
                (center_r, center_g, center_b)
            };
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = 255;
            x += 1;
        }
        y += 1;
    }
    rgba
}

/// Pre-computed 32x32 RGBA byte arrays embedded directly in `.rdata`.
pub static STATUS_RGBA: [[u8; ICON_BUFFER_LEN]; EngineStatus::COUNT] = [
    generate_status_rgba(EngineStatus::Healthy),
    generate_status_rgba(EngineStatus::Degraded),
    generate_status_rgba(EngineStatus::SourceOffline),
    generate_status_rgba(EngineStatus::DestinationOffline),
    generate_status_rgba(EngineStatus::BothOffline),
];

/// Retrieve a reference to the static compile-time RGBA buffer for a status.
#[inline]
#[must_use]
#[allow(dead_code)]
pub fn status_rgba(status: EngineStatus) -> &'static [u8; ICON_BUFFER_LEN] {
    &STATUS_RGBA[status.index()]
}

static ICON_CACHE: OnceLock<[Icon; EngineStatus::COUNT]> = OnceLock::new();

/// Retrieve or initialize a cached tray icon handle with zero-panic fallback.
pub fn get_cached_icon(status: EngineStatus) -> Result<Icon, SyncError> {
    if let Some(cache) = ICON_CACHE.get() {
        return cache
            .get(status.index())
            .cloned()
            .ok_or_else(|| SyncError::tray(format!("No icon cached for {:?}", status)));
    }

    // Build the icon cache array. Return Err without caching if healthy baseline fails.
    let healthy_icon = Icon::from_rgba(
        STATUS_RGBA[EngineStatus::Healthy.index()].to_vec(),
        ICON_SIZE,
        ICON_SIZE,
    )
    .map_err(|e| SyncError::tray_with_source("Failed to create default healthy icon", e))?;

    let mut icons: [Icon; EngineStatus::COUNT] = std::array::from_fn(|_| healthy_icon.clone());
    for &s in &EngineStatus::ALL {
        if s == EngineStatus::Healthy {
            continue;
        }
        match Icon::from_rgba(STATUS_RGBA[s.index()].to_vec(), ICON_SIZE, ICON_SIZE) {
            Ok(icon) => icons[s.index()] = icon,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    status = ?s,
                    "Failed to create status icon; falling back to Healthy icon"
                );
            }
        }
    }

    let _ = ICON_CACHE.set(icons.clone());
    icons
        .get(status.index())
        .cloned()
        .ok_or_else(|| SyncError::tray(format!("No icon cached for {:?}", status)))
}

/// Generate the default system tray icon (Healthy status).
pub fn generate_default_icon() -> Result<Icon, SyncError> {
    get_cached_icon(EngineStatus::Healthy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_generate_status_rgba_buffer_length_and_alpha() {
        for status in EngineStatus::ALL {
            let rgba = generate_status_rgba(status);
            assert_eq!(rgba.len(), 4096);
            for pixel_idx in 0..1024 {
                assert_eq!(rgba[pixel_idx * 4 + 3], 255);
            }
        }
    }

    #[test]
    fn test_generate_status_rgba_border_geometry_boundaries() {
        let rgba = generate_status_rgba(EngineStatus::Healthy);
        let mut border_count = 0usize;
        let mut center_count = 0usize;
        for y in 0..32 {
            for x in 0..32 {
                let is_border = !(4..28).contains(&x) || !(4..28).contains(&y);
                let idx = (y * 32 + x) * 4;
                if is_border {
                    border_count += 1;
                    assert_eq!(&rgba[idx..idx + 4], &[66, 133, 244, 255]);
                } else {
                    center_count += 1;
                    assert_eq!(&rgba[idx..idx + 4], &[255, 255, 255, 255]);
                }
            }
        }
        assert_eq!(border_count, 448);
        assert_eq!(center_count, 576);
    }

    #[test]
    fn test_generate_status_rgba_palette_all_variants() {
        let test_cases = [
            (EngineStatus::Healthy, [66, 133, 244], [255, 255, 255]),
            (EngineStatus::Degraded, [255, 140, 0], [80, 80, 80]),
            (EngineStatus::SourceOffline, [219, 68, 85], [80, 80, 80]),
            (
                EngineStatus::DestinationOffline,
                [244, 180, 0],
                [80, 80, 80],
            ),
            (EngineStatus::BothOffline, [180, 180, 180], [80, 80, 80]),
        ];
        for (status, expected_border_rgb, expected_center_rgb) in test_cases {
            let rgba = generate_status_rgba(status);
            assert_eq!(&rgba[0..3], &expected_border_rgb[..]);
            let center_idx = (16 * 32 + 16) * 4;
            assert_eq!(&rgba[center_idx..center_idx + 3], &expected_center_rgb[..]);
        }
    }

    #[test]
    fn test_status_rgba_matches_direct_call() {
        for status in EngineStatus::ALL {
            let direct = generate_status_rgba(status);
            let static_slice = status_rgba(status);
            assert_eq!(&direct[..], &static_slice[..]);
        }
    }

    #[test]
    fn test_get_cached_icon_all_variants() {
        for status in EngineStatus::ALL {
            let icon_res = get_cached_icon(status);
            assert!(icon_res.is_ok());
        }
        let default_icon_res = generate_default_icon();
        assert!(default_icon_res.is_ok());
    }
}

//! Bounds and fixture checks for the native capture experiment.

pub mod bitstream;
pub mod rtp;
pub mod selection;

/// Center BGRA pixel offset, bounded to this probe's 1024x1024 fixture policy.
pub fn center_offset(width: u32, height: u32, row_pitch: u32) -> Option<usize> {
    if width == 0
        || height == 0
        || width > 1024
        || height > 1024
        || row_pitch < width.checked_mul(4)?
        || row_pitch > 16384
    {
        return None;
    }
    let offset = (height as usize / 2)
        .checked_mul(row_pitch as usize)?
        .checked_add((width as usize / 2).checked_mul(4)?)?;
    let bytes = (height as usize).checked_mul(row_pitch as usize)?;
    (offset.checked_add(4)? <= bytes).then_some(offset)
}

pub fn is_fixture_pixel(bgra: [u8; 4]) -> bool {
    bgra[0].abs_diff(80) <= 12
        && bgra[1].abs_diff(180) <= 12
        && bgra[2].abs_diff(20) <= 12
        && bgra[3] >= 240
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_respects_row_padding() {
        assert_eq!(center_offset(320, 180, 1344), Some(121600));
        assert_eq!(center_offset(480, 270, 1920), Some(260160));
    }

    #[test]
    fn rejects_empty_oversized_and_truncated_rows() {
        for layout in [
            (0, 180, 1280),
            (320, 0, 1280),
            (320, 180, 1279),
            (1025, 20, 4100),
            (20, 1025, 80),
            (320, 180, u32::MAX),
        ] {
            assert_eq!(center_offset(layout.0, layout.1, layout.2), None);
        }
    }

    #[test]
    fn checks_bgra_channels_and_rejects_black_or_swapped_pixels() {
        assert!(is_fixture_pixel([80, 180, 20, 255]));
        assert!(is_fixture_pixel([82, 178, 21, 255]));
        assert!(!is_fixture_pixel([20, 180, 80, 255]));
        assert!(!is_fixture_pixel([0, 0, 0, 255]));
    }
}

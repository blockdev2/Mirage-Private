//! Just enough Annex-B H.264 inspection to decide what a viewer can start on.

/// What an access unit contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitInfo {
    /// Holds an IDR slice (NAL type 5).
    pub keyframe: bool,
    /// Holds both an SPS (7) and a PPS (8).
    pub parameter_sets: bool,
}

/// NAL unit types in Annex-B data, in order. Start codes are `00 00 01` or
/// `00 00 00 01`; bytes before the first start code are ignored.
fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            types.push(data[i + 3] & 0x1f);
            i += 4;
        } else {
            i += 1;
        }
    }
    types
}

pub fn analyze(data: &[u8]) -> UnitInfo {
    let types = nal_types(data);
    UnitInfo {
        keyframe: types.contains(&5),
        parameter_sets: types.contains(&7) && types.contains(&8),
    }
}

/// `header` (SPS and PPS) in front of `unit`.
pub fn with_header(header: &[u8], unit: &[u8]) -> Vec<u8> {
    let mut joined = Vec::with_capacity(header.len() + unit.len());
    joined.extend_from_slice(header);
    joined.extend_from_slice(unit);
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 7] = [0, 0, 0, 1, 0x67, 0x42, 0x1f];
    const PPS: [u8; 6] = [0, 0, 0, 1, 0x68, 0xce];
    const IDR: [u8; 7] = [0, 0, 1, 0x65, 0x88, 0x84, 0x00];
    const P: [u8; 6] = [0, 0, 0, 1, 0x41, 0x9a];

    #[test]
    fn finds_keyframes_and_parameter_sets() {
        let unit = [&SPS[..], &PPS[..], &IDR[..]].concat();
        assert_eq!(
            analyze(&unit),
            UnitInfo {
                keyframe: true,
                parameter_sets: true
            }
        );
    }

    #[test]
    fn a_bare_idr_has_no_parameter_sets_and_a_p_frame_is_no_keyframe() {
        assert_eq!(
            analyze(&IDR),
            UnitInfo {
                keyframe: true,
                parameter_sets: false
            }
        );
        assert_eq!(
            analyze(&P),
            UnitInfo {
                keyframe: false,
                parameter_sets: false
            }
        );
    }

    #[test]
    fn sps_without_pps_does_not_count_as_parameter_sets() {
        let unit = [&SPS[..], &IDR[..]].concat();
        assert!(!analyze(&unit).parameter_sets);
    }

    #[test]
    fn garbage_and_empty_input_are_not_keyframes() {
        assert!(!analyze(&[]).keyframe);
        assert!(!analyze(&[1, 2, 3, 4, 5]).keyframe);
        assert!(!analyze(&[0, 0, 1]).keyframe);
    }

    #[test]
    fn a_prepended_header_makes_a_bare_idr_startable() {
        let header = [&SPS[..], &PPS[..]].concat();
        let unit = with_header(&header, &IDR);
        assert!(analyze(&unit).keyframe && analyze(&unit).parameter_sets);
    }
}

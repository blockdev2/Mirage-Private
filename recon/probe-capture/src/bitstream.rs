//! Dependency-free inspection of H.264/HEVC Annex B and AV1 low-overhead
//! bitstreams. Independent of any decoder: it checks structure and reads the
//! coded dimensions straight from the sequence-level syntax.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|codec| codec.as_str() == name)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
    }
}

/// Facts about one access unit / temporal unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessUnit {
    pub units: usize,
    pub keyframe: bool,
    pub parameter_sets: bool,
    pub dims: Option<(u32, u32)>,
}

/// Split Annex B data on 3- or 4-byte start codes.
pub fn annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (k, &(_, begin)) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |next| next.0);
        while end > begin && data[end - 1] == 0 {
            end -= 1;
        }
        if end > begin {
            nals.push(&data[begin..end]);
        }
    }
    nals
}

/// Remove emulation-prevention bytes (00 00 03 -> 00 00).
pub fn rbsp(nal_payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal_payload.len());
    let mut zeros = 0;
    for &byte in nal_payload {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(bit as u32)
    }

    pub fn bits(&mut self, count: u32) -> Option<u32> {
        if count > 32 {
            return None;
        }
        let mut value: u64 = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()? as u64;
        }
        u32::try_from(value).ok()
    }

    pub fn skip(&mut self, count: usize) -> Option<()> {
        self.pos = self.pos.checked_add(count)?;
        (self.pos <= self.data.len() * 8).then_some(())
    }

    /// Unsigned Exp-Golomb, bounded to 31 leading zeros.
    pub fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)? as u64;
        u32::try_from((1u64 << zeros) - 1 + rest).ok()
    }

    pub fn se(&mut self) -> Option<i64> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
    }
}

/// Display dimensions from an H.264 SPS NAL (including its header byte).
pub fn h264_sps_dims(nal: &[u8]) -> Option<(u32, u32)> {
    if nal.first()? & 0x1f != 7 {
        return None;
    }
    let data = rbsp(&nal[1..]);
    let mut r = BitReader::new(&data);
    let profile = r.bits(8)?;
    r.skip(16)?; // constraint flags + level_idc
    r.ue()?; // seq_parameter_set_id
    let mut chroma = 1;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            return None; // scaling matrices are not needed for this probe
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            let cycle = r.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?;
    let width_mbs = r.ue()?.checked_add(1)?;
    let height_units = r.ue()?.checked_add(1)?;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        r.bit()?;
    }
    r.bit()?; // direct_8x8_inference
    let (mut left, mut right, mut top, mut bottom) = (0, 0, 0, 0);
    if r.bit()? == 1 {
        left = r.ue()?;
        right = r.ue()?;
        top = r.ue()?;
        bottom = r.ue()?;
    }
    let (crop_x, crop_y) = match chroma {
        0 => (1, 2 - frame_mbs_only),
        1 => (2, 2 * (2 - frame_mbs_only)),
        2 => (2, 2 - frame_mbs_only),
        _ => (1, 2 - frame_mbs_only),
    };
    let width = (width_mbs * 16).checked_sub(crop_x * (left + right))?;
    let height = ((2 - frame_mbs_only) * height_units * 16).checked_sub(crop_y * (top + bottom))?;
    Some((width, height))
}

/// Display dimensions from an HEVC SPS NAL (including its two header bytes).
pub fn hevc_sps_dims(nal: &[u8]) -> Option<(u32, u32)> {
    if (nal.first()? >> 1) & 0x3f != 33 || nal.len() < 3 {
        return None;
    }
    let data = rbsp(&nal[2..]);
    let mut r = BitReader::new(&data);
    r.bits(4)?; // vps id
    let sub_layers = r.bits(3)? as usize;
    r.bit()?;
    // profile_tier_level(1, sub_layers)
    r.skip(88)?;
    r.skip(8)?;
    let mut present = [(false, false); 8];
    for flags in present.iter_mut().take(sub_layers) {
        *flags = (r.bit()? == 1, r.bit()? == 1);
    }
    if sub_layers > 0 {
        r.skip(2 * (8 - sub_layers))?;
    }
    for &(profile, level) in present.iter().take(sub_layers) {
        if profile {
            r.skip(88)?;
        }
        if level {
            r.skip(8)?;
        }
    }
    r.ue()?; // sps id
    let chroma = r.ue()?;
    if chroma == 3 {
        r.bit()?;
    }
    let width = r.ue()?;
    let height = r.ue()?;
    let (sub_w, sub_h) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    if r.bit()? == 1 {
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        return Some((
            width.checked_sub(sub_w * (left + right))?,
            height.checked_sub(sub_h * (top + bottom))?,
        ));
    }
    Some((width, height))
}

/// One AV1 OBU: (type, header bytes, payload).
pub struct Obu<'a> {
    pub kind: u8,
    pub header: &'a [u8],
    pub payload: &'a [u8],
}

pub fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &byte) in data.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

pub fn write_leb128(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Parse a low-overhead (size-delimited) AV1 OBU sequence.
pub fn av1_obus(mut data: &[u8]) -> Option<Vec<Obu<'_>>> {
    let mut obus = Vec::new();
    while !data.is_empty() {
        let first = data[0];
        if first & 0x80 != 0 {
            return None; // forbidden bit
        }
        let header_len = if first & 0x04 != 0 { 2 } else { 1 };
        let header = data.get(..header_len)?;
        let (payload_len, size_len) = if first & 0x02 != 0 {
            let (len, used) = leb128(data.get(header_len..)?)?;
            (usize::try_from(len).ok()?, used)
        } else {
            (data.len() - header_len, 0)
        };
        let start = header_len + size_len;
        let payload = data.get(start..start.checked_add(payload_len)?)?;
        obus.push(Obu {
            kind: (first >> 3) & 0x0f,
            header,
            payload,
        });
        data = &data[start + payload_len..];
    }
    Some(obus)
}

/// Max frame dimensions from an AV1 sequence header payload. Returns `None`
/// for timing/decoder-model variants this probe does not need to parse.
pub fn av1_sequence_dims(payload: &[u8]) -> Option<(u32, u32)> {
    let mut r = BitReader::new(payload);
    r.bits(3)?; // seq_profile
    r.bit()?; // still_picture
    if r.bit()? == 1 {
        r.bits(5)?;
    } else {
        if r.bit()? == 1 {
            return None; // timing_info_present
        }
        let initial_delay = r.bit()?;
        let points = r.bits(5)? + 1;
        for _ in 0..points {
            r.bits(12)?;
            if r.bits(5)? > 7 {
                r.bit()?;
            }
            if initial_delay == 1 && r.bit()? == 1 {
                r.bits(4)?;
            }
        }
    }
    let width_bits = r.bits(4)? + 1;
    let height_bits = r.bits(4)? + 1;
    Some((r.bits(width_bits)? + 1, r.bits(height_bits)? + 1))
}

/// Re-serialize AV1 OBUs with explicit size fields and without temporal
/// delimiters: the canonical form used to compare RTP round trips.
pub fn av1_canonical(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    for obu in av1_obus(data)? {
        if obu.kind == 2 {
            continue;
        }
        out.push(obu.header[0] | 0x02);
        out.extend_from_slice(&obu.header[1..]);
        write_leb128(obu.payload.len(), &mut out);
        out.extend_from_slice(obu.payload);
    }
    Some(out)
}

/// Structural summary of one encoded unit. `None` means malformed.
pub fn inspect(codec: Codec, data: &[u8]) -> Option<AccessUnit> {
    let mut au = AccessUnit::default();
    let mut vcl = false;
    match codec {
        Codec::H264 => {
            let (mut sps, mut pps) = (false, false);
            for nal in annexb_nals(data) {
                if nal[0] & 0x80 != 0 {
                    return None;
                }
                au.units += 1;
                match nal[0] & 0x1f {
                    1 => vcl = true,
                    5 => {
                        vcl = true;
                        au.keyframe = true;
                    }
                    7 => {
                        sps = true;
                        au.dims = au.dims.or(h264_sps_dims(nal));
                    }
                    8 => pps = true,
                    _ => {}
                }
            }
            au.parameter_sets = sps && pps;
        }
        Codec::Hevc => {
            let (mut vps, mut sps, mut pps) = (false, false, false);
            for nal in annexb_nals(data) {
                if nal.len() < 2 || nal[0] & 0x80 != 0 {
                    return None;
                }
                au.units += 1;
                match (nal[0] >> 1) & 0x3f {
                    0..=15 => vcl = true,
                    16..=23 => {
                        vcl = true;
                        au.keyframe = true;
                    }
                    24..=31 => vcl = true,
                    32 => vps = true,
                    33 => {
                        sps = true;
                        au.dims = au.dims.or(hevc_sps_dims(nal));
                    }
                    34 => pps = true,
                    _ => {}
                }
            }
            au.parameter_sets = vps && sps && pps;
        }
        Codec::Av1 => {
            let mut frame_seen = false;
            for obu in av1_obus(data)? {
                au.units += 1;
                match obu.kind {
                    1 => {
                        au.parameter_sets = true;
                        au.dims = au.dims.or(av1_sequence_dims(obu.payload));
                    }
                    3 | 6 => {
                        vcl = true;
                        if !frame_seen {
                            frame_seen = true;
                            let mut r = BitReader::new(obu.payload);
                            // show_existing_frame == 0 && frame_type == KEY_FRAME
                            au.keyframe = r.bit() == Some(0) && r.bits(2) == Some(0);
                        }
                    }
                    4 => vcl = true,
                    _ => {}
                }
            }
        }
    }
    vcl.then_some(au)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal MSB-first bit writer for building syntax fixtures.
    #[derive(Default)]
    pub struct BitWriter {
        pub bytes: Vec<u8>,
        bits: usize,
    }

    impl BitWriter {
        pub fn put(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                if self.bits.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                *self.bytes.last_mut().unwrap() |= bit << (7 - self.bits % 8);
                self.bits += 1;
            }
        }
        pub fn ue(&mut self, value: u32) {
            let v = value as u64 + 1;
            let len = 64 - v.leading_zeros();
            self.put(0, len - 1);
            self.put(v as u32, len);
        }
        pub fn finish(mut self) -> Vec<u8> {
            self.put(1, 1); // rbsp_stop_one_bit
            self.bytes
        }
    }

    /// High-profile 4:2:0 SPS for 640x360 coded as 640x368 with bottom crop.
    pub fn h264_sps_640x360() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.put(100, 8);
        w.put(0, 8);
        w.put(31, 8);
        w.ue(0);
        w.ue(1); // chroma 4:2:0
        w.ue(0);
        w.ue(0);
        w.put(0, 1);
        w.put(0, 1); // no scaling matrix
        w.ue(0);
        w.ue(0); // poc type 0
        w.ue(4);
        w.ue(1);
        w.put(0, 1);
        w.ue(39); // 40 MBs wide
        w.ue(22); // 23 MBs high = 368
        w.put(1, 1); // frame_mbs_only
        w.put(1, 1);
        w.put(1, 1); // cropping
        w.ue(0);
        w.ue(0);
        w.ue(0);
        w.ue(4); // 8 luma rows
        let mut nal = vec![0x67];
        nal.extend(w.finish());
        nal
    }

    pub fn hevc_sps_640x360() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.put(0, 4);
        w.put(0, 3);
        w.put(1, 1);
        for _ in 0..11 {
            w.put(0, 8); // 88 general profile bits
        }
        w.put(93, 8); // level
        w.ue(0);
        w.ue(1); // 4:2:0
        w.ue(640);
        w.ue(368);
        w.put(1, 1);
        w.ue(0);
        w.ue(0);
        w.ue(0);
        w.ue(4); // 8 luma rows
        let mut nal = vec![33 << 1, 1];
        nal.extend(w.finish());
        nal
    }

    pub fn av1_sequence_header_640x360() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.put(0, 3);
        w.put(0, 1);
        w.put(0, 1);
        w.put(0, 1); // no timing info
        w.put(0, 1); // no initial display delay
        w.put(0, 5); // one operating point
        w.put(0, 12);
        w.put(8, 5); // level > 7 -> tier bit
        w.put(0, 1);
        w.put(9, 4); // 10 width bits
        w.put(8, 4); // 9 height bits
        w.put(639, 10);
        w.put(359, 9);
        w.finish()
    }

    pub fn av1_unit(frame_payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x12, 0x00]; // temporal delimiter with size 0
        let seq = av1_sequence_header_640x360();
        out.push(0x0a);
        write_leb128(seq.len(), &mut out);
        out.extend(&seq);
        out.push(0x32); // frame OBU with size
        write_leb128(frame_payload.len(), &mut out);
        out.extend(frame_payload);
        out
    }

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9,
        ];
        let nals = annexb_nals(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 9]]);
    }

    #[test]
    fn removes_emulation_prevention() {
        assert_eq!(rbsp(&[0, 0, 3, 1, 0, 0, 3, 0]), vec![0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn reads_cropped_h264_and_hevc_dimensions() {
        assert_eq!(h264_sps_dims(&h264_sps_640x360()), Some((640, 360)));
        assert_eq!(hevc_sps_dims(&hevc_sps_640x360()), Some((640, 360)));
    }

    #[test]
    fn summarizes_h264_and_hevc_keyframes() {
        let mut h264 = vec![0, 0, 0, 1];
        h264.extend(h264_sps_640x360());
        h264.extend([0, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x88, 0x84]);
        let au = inspect(Codec::H264, &h264).unwrap();
        assert!(au.keyframe && au.parameter_sets);
        assert_eq!(au.dims, Some((640, 360)));
        assert!(
            !inspect(Codec::H264, &[0, 0, 1, 0x41, 0x9a])
                .unwrap()
                .keyframe
        );
        assert!(
            inspect(Codec::H264, &[0, 0, 1, 0x67, 1]).is_none(),
            "no VCL"
        );

        let mut hevc = vec![0, 0, 0, 1, 32 << 1, 1, 0x0c, 0, 0, 0, 1];
        hevc.extend(hevc_sps_640x360());
        hevc.extend([0, 0, 1, 34 << 1, 1, 0xc0, 0, 0, 1, 19 << 1, 1, 0xaf]);
        let au = inspect(Codec::Hevc, &hevc).unwrap();
        assert!(au.keyframe && au.parameter_sets);
        assert_eq!(au.dims, Some((640, 360)));
    }

    #[test]
    fn summarizes_av1_temporal_units() {
        let unit = av1_unit(&[0x10, 0xaa, 0xbb]); // show_existing=0, KEY_FRAME
        let au = inspect(Codec::Av1, &unit).unwrap();
        assert!(au.keyframe && au.parameter_sets);
        assert_eq!(au.dims, Some((640, 360)));
        let inter = [0x32, 0x02, 0x30, 0x00]; // frame_type = INTER
        assert!(!inspect(Codec::Av1, &inter).unwrap().keyframe);
        assert!(
            inspect(Codec::Av1, &[0x32, 0x05, 0x00]).is_none(),
            "truncated"
        );
        assert!(
            inspect(Codec::Av1, &[0x92, 0x00]).is_none(),
            "forbidden bit"
        );
        let canonical = av1_canonical(&unit).unwrap();
        assert_eq!(canonical[0] >> 3 & 0x0f, 1, "temporal delimiter dropped");
    }

    #[test]
    fn exp_golomb_rejects_unbounded_prefix() {
        assert_eq!(BitReader::new(&[0; 8]).ue(), None);
        let mut r = BitReader::new(&[0b0010_1100]);
        assert_eq!((r.ue(), r.ue()), (Some(4), Some(0)));
    }
}

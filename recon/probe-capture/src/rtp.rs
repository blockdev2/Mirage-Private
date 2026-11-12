//! RTP payload formats for the probe's encoded units, with matching
//! depacketizers so every run proves a lossless round trip:
//! H.264 (RFC 6184, single NAL + FU-A), HEVC (RFC 7798, single NAL + FU) and
//! AV1 (AOM "RTP Payload Format for AV1", one OBU element per packet + Z/Y
//! fragmentation, N flag on a new coded video sequence).

use crate::bitstream::{Codec, annexb_nals, av1_obus, write_leb128};

pub const RTP_HEADER_LEN: usize = 12;

/// Split one encoded unit into RTP payloads no larger than `max_payload`.
pub fn payloads(codec: Codec, unit: &[u8], max_payload: usize) -> Option<Vec<Vec<u8>>> {
    if max_payload < 16 {
        return None;
    }
    let mut out = Vec::new();
    match codec {
        Codec::H264 => {
            for nal in annexb_nals(unit) {
                if nal.len() <= max_payload {
                    out.push(nal.to_vec());
                    continue;
                }
                let indicator = (nal[0] & 0xe0) | 28;
                fragment(&nal[1..], max_payload - 2, &mut out, |first, last| {
                    let header = (u8::from(first) << 7) | (u8::from(last) << 6) | (nal[0] & 0x1f);
                    vec![indicator, header]
                });
            }
        }
        Codec::Hevc => {
            for nal in annexb_nals(unit) {
                if nal.len() < 2 {
                    return None;
                }
                if nal.len() <= max_payload {
                    out.push(nal.to_vec());
                    continue;
                }
                let kind = (nal[0] >> 1) & 0x3f;
                let header = [(nal[0] & 0x81) | (49 << 1), nal[1]];
                fragment(&nal[2..], max_payload - 3, &mut out, |first, last| {
                    let fu = (u8::from(first) << 7) | (u8::from(last) << 6) | kind;
                    vec![header[0], header[1], fu]
                });
            }
        }
        Codec::Av1 => {
            let obus = av1_obus(unit)?;
            let new_sequence = obus.iter().any(|obu| obu.kind == 1);
            let mut first_packet = true;
            for obu in obus.iter().filter(|obu| obu.kind != 2) {
                // OBU element without obu_size (has_size_field cleared).
                let mut element = vec![obu.header[0] & !0x02];
                element.extend_from_slice(&obu.header[1..]);
                element.extend_from_slice(obu.payload);
                fragment(&element, max_payload - 1, &mut out, |first, last| {
                    let n = first && first_packet && new_sequence;
                    // Z: continues a fragment; Y: continues in next; W=1 element.
                    vec![
                        (u8::from(!first) << 7)
                            | (u8::from(!last) << 6)
                            | (1 << 4)
                            | (u8::from(n) << 3),
                    ]
                });
                first_packet = false;
            }
        }
    }
    Some(out)
}

fn fragment(
    data: &[u8],
    chunk: usize,
    out: &mut Vec<Vec<u8>>,
    header: impl Fn(bool, bool) -> Vec<u8>,
) {
    let count = data.len().div_ceil(chunk).max(1);
    for (i, piece) in data.chunks(chunk).enumerate() {
        let mut packet = header(i == 0, i + 1 == count);
        packet.extend_from_slice(piece);
        out.push(packet);
    }
}

/// Reassemble payloads into canonical form: 4-byte Annex B start codes for
/// H.264/HEVC, size-delimited OBUs (no temporal delimiter) for AV1.
pub fn depacketize(codec: Codec, packets: &[Vec<u8>]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    for p in packets {
        match codec {
            Codec::H264 => {
                if p.first()? & 0x1f == 28 {
                    let (indicator, fu) = (p[0], *p.get(1)?);
                    if fu & 0x80 != 0 {
                        pending = Some(vec![(indicator & 0xe0) | (fu & 0x1f)]);
                    }
                    pending.as_mut()?.extend_from_slice(&p[2..]);
                    if fu & 0x40 != 0 {
                        push_annexb(&mut out, &pending.take()?);
                    }
                } else {
                    push_annexb(&mut out, p);
                }
            }
            Codec::Hevc => {
                if (p.first()? >> 1) & 0x3f == 49 {
                    let fu = *p.get(2)?;
                    if fu & 0x80 != 0 {
                        pending = Some(vec![(p[0] & 0x81) | ((fu & 0x3f) << 1), p[1]]);
                    }
                    pending.as_mut()?.extend_from_slice(&p[3..]);
                    if fu & 0x40 != 0 {
                        push_annexb(&mut out, &pending.take()?);
                    }
                } else {
                    push_annexb(&mut out, p);
                }
            }
            Codec::Av1 => {
                let aggregation = *p.first()?;
                if (aggregation >> 4) & 0x03 != 1 {
                    return None; // this depacketizer accepts W=1 only
                }
                if aggregation & 0x80 == 0 {
                    if pending.is_some() {
                        return None;
                    }
                    pending = Some(Vec::new());
                }
                pending.as_mut()?.extend_from_slice(&p[1..]);
                if aggregation & 0x40 == 0 {
                    let element = pending.take()?;
                    let header_len = if element.first()? & 0x04 != 0 { 2 } else { 1 };
                    let header = element.get(..header_len)?;
                    out.push(header[0] | 0x02);
                    out.extend_from_slice(&header[1..]);
                    write_leb128(element.len() - header_len, &mut out);
                    out.extend_from_slice(&element[header_len..]);
                }
            }
        }
    }
    pending.is_none().then_some(out)
}

fn push_annexb(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

/// Annex B with uniform 4-byte start codes, for comparing round trips.
pub fn annexb_canonical(unit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(unit.len() + 8);
    for nal in annexb_nals(unit) {
        push_annexb(&mut out, nal);
    }
    out
}

/// Minimal RTP header (RFC 3550, no CSRC/extension); marker on the last packet.
pub fn rtp_packet(
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    marker: bool,
    payload: &[u8],
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(RTP_HEADER_LEN + payload.len());
    packet.push(0x80);
    packet.push((u8::from(marker) << 7) | (payload_type & 0x7f));
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

/// Parse an RTP packet (RFC 3550), skipping CSRCs, header extension and padding.
pub fn parse_rtp(packet: &[u8]) -> Option<(RtpHeader, &[u8])> {
    let first = *packet.first()?;
    if first >> 6 != 2 || packet.len() < RTP_HEADER_LEN {
        return None;
    }
    let mut start = RTP_HEADER_LEN + 4 * usize::from(first & 0x0f);
    if first & 0x10 != 0 {
        let words = u16::from_be_bytes([*packet.get(start + 2)?, *packet.get(start + 3)?]);
        start += 4 + 4 * usize::from(words);
    }
    let mut end = packet.len();
    if first & 0x20 != 0 {
        end = end.checked_sub(usize::from(*packet.last()?))?;
    }
    let payload = packet.get(start..end)?;
    let header = RtpHeader {
        marker: packet[1] & 0x80 != 0,
        payload_type: packet[1] & 0x7f,
        sequence: u16::from_be_bytes([packet[2], packet[3]]),
        timestamp: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
        ssrc: u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
    };
    Some((header, payload))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiveStats {
    pub packets: u64,
    pub lost: u64,
    pub late_or_duplicate: u64,
    pub foreign_ssrc: u64,
    pub malformed: u64,
    pub units: u64,
    /// Units incomplete or undecodable because of loss.
    pub dropped_units: u64,
    /// Complete units withheld until the next keyframe after a loss.
    pub awaiting_keyframe: u64,
}

const MAX_PACKETS_PER_UNIT: usize = 4096;
const MAX_UNIT_BYTES: usize = 8 * 1024 * 1024;

/// Receiver-side access-unit reassembly. Units are delimited by timestamp and
/// the marker bit. After any loss, output resumes only at a keyframe, which is
/// where a production receiver would also send a keyframe request (PLI).
pub struct Reassembler {
    codec: Codec,
    ssrc: Option<u32>,
    expected: Option<u16>,
    current: Option<(u32, Vec<Vec<u8>>, bool)>,
    need_keyframe: bool,
    pub stats: ReceiveStats,
}

impl Reassembler {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            ssrc: None,
            expected: None,
            current: None,
            need_keyframe: true,
            stats: ReceiveStats::default(),
        }
    }

    pub fn push(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        let Some((header, payload)) = parse_rtp(packet) else {
            self.stats.malformed += 1;
            return None;
        };
        if *self.ssrc.get_or_insert(header.ssrc) != header.ssrc {
            self.stats.foreign_ssrc += 1;
            return None;
        }
        if let Some(expected) = self.expected {
            let gap = header.sequence.wrapping_sub(expected) as i16;
            if gap < 0 {
                self.stats.late_or_duplicate += 1;
                return None;
            }
            if gap > 0 {
                self.stats.lost += gap as u64;
                self.need_keyframe = true;
                if let Some(current) = self.current.as_mut() {
                    current.2 = true;
                }
            }
        }
        self.expected = Some(header.sequence.wrapping_add(1));
        self.stats.packets += 1;
        if self
            .current
            .as_ref()
            .is_some_and(|(timestamp, _, _)| *timestamp != header.timestamp)
        {
            self.current = None; // previous unit never saw its marker
            self.stats.dropped_units += 1;
        }
        let (_, parts, broken) = self
            .current
            .get_or_insert_with(|| (header.timestamp, Vec::new(), false));
        let size: usize = parts.iter().map(Vec::len).sum();
        if parts.len() >= MAX_PACKETS_PER_UNIT || size + payload.len() > MAX_UNIT_BYTES {
            *broken = true;
        } else {
            parts.push(payload.to_vec());
        }
        if !header.marker {
            return None;
        }
        let (_, parts, broken) = self.current.take()?;
        let unit = (!broken).then(|| depacketize(self.codec, &parts)).flatten();
        let Some(unit) = unit else {
            self.stats.dropped_units += 1;
            self.need_keyframe = true;
            return None;
        };
        let keyframe = crate::bitstream::inspect(self.codec, &unit).is_some_and(|au| au.keyframe);
        if self.need_keyframe && !keyframe {
            self.stats.awaiting_keyframe += 1;
            return None;
        }
        self.need_keyframe = false;
        self.stats.units += 1;
        Some(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::{av1_canonical, tests::*};

    fn h264_stream() -> Vec<Vec<u8>> {
        // keyframe (SPS+PPS+large IDR), then two inter frames, then keyframe.
        let key = |len| {
            let mut unit = vec![0, 0, 0, 1];
            unit.extend(h264_sps_640x360());
            unit.extend([0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1]);
            unit.extend(big_nal(&[0x65], len));
            unit
        };
        let inter = |len| {
            let mut unit = vec![0, 0, 0, 1];
            unit.extend(big_nal(&[0x41], len));
            unit
        };
        vec![key(3000), inter(1500), inter(200), key(2500)]
    }

    fn packetize(units: &[Vec<u8>], first_sequence: u16) -> Vec<Vec<Vec<u8>>> {
        let mut sequence = first_sequence;
        units
            .iter()
            .enumerate()
            .map(|(frame, unit)| {
                let parts = payloads(Codec::H264, unit, 500).unwrap();
                let count = parts.len();
                parts
                    .iter()
                    .enumerate()
                    .map(|(i, part)| {
                        let packet =
                            rtp_packet(96, sequence, frame as u32 * 4500, 9, i + 1 == count, part);
                        sequence = sequence.wrapping_add(1);
                        packet
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn reassembles_in_order_stream_across_sequence_wrap() {
        let units = h264_stream();
        let mut receiver = Reassembler::new(Codec::H264);
        let mut out = Vec::new();
        for packet in packetize(&units, 65530).concat() {
            out.extend(receiver.push(&packet));
        }
        assert_eq!(
            out,
            units
                .iter()
                .map(|u| annexb_canonical(u))
                .collect::<Vec<_>>()
        );
        assert_eq!((receiver.stats.lost, receiver.stats.units), (0, 4));
    }

    #[test]
    fn loss_drops_unit_and_waits_for_next_keyframe() {
        let units = h264_stream();
        let mut packets = packetize(&units, 100);
        packets[1].remove(1); // lose a fragment of the first inter frame
        let mut receiver = Reassembler::new(Codec::H264);
        let out: Vec<_> = packets
            .concat()
            .iter()
            .filter_map(|p| receiver.push(p))
            .collect();
        assert_eq!(out.len(), 2, "first keyframe and the recovery keyframe");
        assert_eq!(out[1], annexb_canonical(&units[3]));
        let stats = receiver.stats;
        assert_eq!(
            (stats.lost, stats.dropped_units, stats.awaiting_keyframe),
            (1, 1, 1)
        );
    }

    #[test]
    fn ignores_duplicates_foreign_ssrc_and_garbage() {
        let units = h264_stream();
        let packets = packetize(&units[..1], 0).concat();
        let mut receiver = Reassembler::new(Codec::H264);
        receiver.push(&packets[0]);
        receiver.push(&packets[0]);
        let mut foreign = packets[1].clone();
        foreign[11] ^= 1;
        receiver.push(&foreign);
        receiver.push(&[0x00, 1, 2]);
        let out: Vec<_> = packets[1..]
            .iter()
            .filter_map(|p| receiver.push(p))
            .collect();
        assert_eq!(out.len(), 1);
        let stats = receiver.stats;
        assert_eq!(
            (stats.late_or_duplicate, stats.foreign_ssrc, stats.malformed),
            (1, 1, 1)
        );
    }

    #[test]
    fn parses_csrc_extension_and_padding() {
        let mut packet = vec![0xb1, 0x60, 0, 7, 0, 0, 0, 9, 0, 0, 0, 5];
        packet.extend([1, 2, 3, 4]); // one CSRC
        packet.extend([0xbe, 0xde, 0, 1, 9, 9, 9, 9]); // one-word extension
        packet.extend([0xaa, 0xbb, 0, 0, 3]); // payload + 3 padding bytes
        let (header, payload) = parse_rtp(&packet).unwrap();
        assert_eq!((header.sequence, header.timestamp, header.ssrc), (7, 9, 5));
        assert_eq!(payload, [0xaa, 0xbb]);
        assert!(parse_rtp(&[0x40; 12]).is_none(), "wrong version");
    }

    fn big_nal(header: &[u8], len: usize) -> Vec<u8> {
        let mut nal = header.to_vec();
        nal.extend((0..len).map(|i| (i % 251 + 1) as u8));
        nal
    }

    #[test]
    fn h264_fragments_and_reassembles_losslessly() {
        let mut unit = vec![0, 0, 1];
        unit.extend(h264_sps_640x360());
        unit.extend([0, 0, 0, 1]);
        unit.extend(big_nal(&[0x65], 5000));
        let packets = payloads(Codec::H264, &unit, 1188).unwrap();
        assert!(packets.iter().all(|p| p.len() <= 1188));
        assert!(packets.len() > 4);
        assert_eq!(packets[1][0] & 0x1f, 28);
        assert_eq!(
            depacketize(Codec::H264, &packets).unwrap(),
            annexb_canonical(&unit)
        );
    }

    #[test]
    fn hevc_fragments_and_reassembles_losslessly() {
        let mut unit = vec![0, 0, 0, 1];
        unit.extend(hevc_sps_640x360());
        unit.extend([0, 0, 0, 1]);
        unit.extend(big_nal(&[19 << 1, 1], 4000));
        let packets = payloads(Codec::Hevc, &unit, 1188).unwrap();
        assert!(packets.iter().all(|p| p.len() <= 1188));
        assert_eq!((packets[1][0] >> 1) & 0x3f, 49);
        assert_eq!(
            depacketize(Codec::Hevc, &packets).unwrap(),
            annexb_canonical(&unit)
        );
    }

    #[test]
    fn av1_fragments_marks_new_sequence_and_reassembles() {
        let frame: Vec<u8> = std::iter::once(0x10)
            .chain((0..3000).map(|i| i as u8))
            .collect();
        let unit = av1_unit(&frame);
        let packets = payloads(Codec::Av1, &unit, 1188).unwrap();
        assert!(packets.iter().all(|p| p.len() <= 1188));
        assert_eq!(
            packets[0][0] & 0x08,
            0x08,
            "N set on new coded video sequence"
        );
        assert!(packets[1..].iter().all(|p| p[0] & 0x08 == 0));
        assert_eq!(packets[0][1] & 0x02, 0, "obu_size removed inside RTP");
        assert_eq!(
            depacketize(Codec::Av1, &packets).unwrap(),
            av1_canonical(&unit).unwrap()
        );
    }

    #[test]
    fn rejects_dangling_fragments_and_tiny_mtu() {
        let unit = [0, 0, 1, 0x65]
            .iter()
            .copied()
            .chain(std::iter::repeat_n(7, 100))
            .collect::<Vec<_>>();
        let mut packets = payloads(Codec::H264, &unit, 40).unwrap();
        packets.pop();
        assert!(depacketize(Codec::H264, &packets).is_none());
        assert!(payloads(Codec::H264, &unit, 8).is_none());
    }

    #[test]
    fn writes_rtp_header() {
        let packet = rtp_packet(96, 0x1234, 90_000, 7, true, &[0xaa]);
        assert_eq!(
            packet,
            [0x80, 0xe0, 0x12, 0x34, 0, 1, 0x5f, 0x90, 0, 0, 0, 7, 0xaa]
        );
    }
}

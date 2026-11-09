//! MPEG-TS demuxing and muxing (ISO/IEC 13818-1).
//!
//! A transport stream is 188-byte packets starting with the sync
//! byte `0x47`. This module parses PAT/PMT tables, assembles PES
//! packets per PID, reads PCR timestamps, and checks continuity
//! counters. A continuity gap drops the partial PES being
//! assembled (counted in `TsStreamStats`, not silently completed);
//! a cut-short final packet is `Truncated`; a packet that fails
//! the sync check is `InvalidData`. Stream types `0x1B` (AVC),
//! `0x24` (HEVC), and `0x0F` (AAC in ADTS) are demuxed as
//! passthrough packets behind the license gate in `hw`; MP3
//! (stream types `0x03` MPEG-1 audio and `0x04` MPEG-2 audio)
//! is demuxed as plain `Codec::Mp3` packets with no license
//! gate. Other stream types are listed with `codec: None` and
//! yield no packets.
//!
//! The muxer writes PAT/PMT once, then one PES per packet, split
//! across 188-byte packets with continuity counters and a PCR on
//! the first packet of each PES that carries a timestamp. Video
//! PES packets use `PES_packet_length` 0 (unbounded, per the
//! spec); audio PES packets use the real length when it fits.

use crate::error::{Error, Result};
use crate::frame::{Codec, Packet};
use crate::hw::{licensed_feature_enabled, passthrough_allowed};
use crate::license::LicenseContext;
use crate::limits::{MAX_AUDIO_PACKET_BYTES, MAX_MPEGTS_FILE_BYTES, MAX_VIDEO_PACKET_BYTES};
use std::collections::HashMap;

/// MPEG-TS packet length in bytes (ISO/IEC 13818-1 section 2.4.3).
pub const TS_PACKET_LEN: usize = 188;

/// MPEG-TS sync byte.
pub const TS_SYNC_BYTE: u8 = 0x47;

/// PAT PID.
pub const TS_PAT_PID: u16 = 0;

/// Stream type for H.264/AVC (ISO/IEC 13818-1 Table 2-34).
pub const TS_STREAM_TYPE_AVC: u8 = 0x1B;

/// Stream type for H.265/HEVC.
pub const TS_STREAM_TYPE_HEVC: u8 = 0x24;

/// Stream type for AAC in ADTS (ISO/IEC 13818-7).
pub const TS_STREAM_TYPE_AAC: u8 = 0x0F;

/// Stream type for MP3 (ISO/IEC 11172-3 / 13818-3, MPEG-1 audio).
pub const TS_STREAM_TYPE_MP3: u8 = 0x03;

/// Stream type for MPEG-2 audio (ISO/IEC 13818-3), also used for
/// MP3 (Layer III) in practice.
pub const TS_STREAM_TYPE_MP3_MPEG2: u8 = 0x04;

/// MPEG-2 CRC-32 (ISO/IEC 13818-1 section 2.4.3.1): polynomial
/// `0x04C11DB7`, initial value `0xFFFFFFFF`, MSB-first, no final
/// xor. Check value: `0x0376E6E7` for `"123456789"`.
#[must_use]
pub fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// One elementary stream from the PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsStream {
    /// Elementary PID.
    pub pid: u16,
    /// Stream type byte from the PMT.
    pub stream_type: u8,
    /// Library codec, or `None` for a stream type this library
    /// does not demux (for example private data).
    pub codec: Option<Codec>,
    /// Last PCR seen for this PID, in microseconds.
    pub last_pcr_us: Option<u64>,
}

/// Counters for what the demuxer saw while parsing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TsStreamStats {
    /// TS packets read.
    pub packets: usize,
    /// Continuity-counter gaps (a partial PES was dropped).
    pub continuity_errors: usize,
    /// Duplicate packets skipped (same continuity counter).
    pub dropped_duplicates: usize,
    /// Incomplete PES packets dropped at the next start or EOF.
    pub incomplete_pes_dropped: usize,
    /// PES packets shorter than their declared length, dropped.
    pub truncated_pes_dropped: usize,
}

/// Per-PID assembly state while parsing.
#[derive(Default)]
struct PidState {
    last_cc: Option<u8>,
    /// Accumulated PES payload bytes (header included).
    buffer: Vec<u8>,
    /// Parsed PES header, once the buffer holds it.
    header: Option<PesHeader>,
    /// PTS of the PES being assembled, in microseconds.
    pts_us: Option<u64>,
    /// `random_access_indicator` of the packet that started it.
    random_access: bool,
    last_pcr_us: Option<u64>,
    /// File offset of the TS packet that started this PES.
    /// Packets are reordered by this offset at the end of
    /// parsing so `next_packet` yields them in file order:
    /// a declared-length PES can otherwise be emitted before
    /// an unbounded PES that started earlier.
    pes_start: usize,
}

/// Parsed PES header fields (ISO/IEC 13818-1 section 2.4.3.6).
struct PesHeader {
    /// Declared `PES_packet_length` (0 means unbounded).
    packet_length: u16,
    /// Total header length in bytes (9 + `header_data_length`).
    header_len: usize,
}

/// Demuxer for an MPEG-TS stream held in memory. Packets are
/// parsed at construction; `next_packet` yields them in file
/// order, gated by the license context for AVC/HEVC/AAC.
#[derive(Debug)]
pub struct TsDemuxer {
    streams: Vec<TsStream>,
    packets: Vec<Packet>,
    packet_pids: Vec<u16>,
    position: usize,
    stats: TsStreamStats,
    license: Option<LicenseContext>,
}

impl TsDemuxer {
    /// Reads and demuxes an MPEG-TS stream from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short packet or section,
    /// `InvalidData` for a missing sync byte, a missing PAT/PMT,
    /// a scrambled packet, or a corrupt PES header, `Unsupported`
    /// for no licensed path (never raised here: unknown streams
    /// are skipped, not errors), and `AllocationLimit` for a
    /// stream above the MPEG-TS file limit.
    pub fn new<R: std::io::Read>(reader: R) -> Result<Self> {
        use crate::byte_source::BoundedReader;
        let mut reader = BoundedReader::new(reader, MAX_MPEGTS_FILE_BYTES)?;
        let bytes = reader.read_all_bounded()?;
        Self::parse(&bytes)
    }

    /// Parses an already-read MPEG-TS stream.
    ///
    /// # Errors
    ///
    /// The same errors as `new`.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        parse_ts(bytes)
    }

    /// Elementary streams from the PMT, in PMT order.
    #[must_use]
    pub fn streams(&self) -> &[TsStream] {
        &self.streams
    }

    /// Parse counters.
    #[must_use]
    pub fn stats(&self) -> TsStreamStats {
        self.stats
    }

    /// Number of packets in the stream.
    #[must_use]
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// PID of the packet at `position`, for callers that need the
    /// stream a packet came from.
    #[must_use]
    pub fn packet_pid(&self, position: usize) -> Option<u16> {
        self.packet_pids.get(position).copied()
    }

    /// Sets the license context for passthrough of AVC/HEVC/AAC.
    /// The license is validated eagerly for every licensed codec
    /// the stream carries, and is stored only when every check
    /// passes; a failed check leaves the previous context (or
    /// none) in place.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the feature for a carried
    /// licensed codec is off, `LicenseRequired` when the license
    /// has expired, and `InvalidArgument` for a malformed
    /// license.
    pub fn set_license_context(&mut self, mut license: Option<LicenseContext>) -> Result<()> {
        if let Some(context) = license.as_mut() {
            for stream in &self.streams {
                if let Some(codec) = stream.codec {
                    if matches!(codec, Codec::Avc | Codec::Hevc | Codec::Aac) {
                        if !licensed_feature_enabled(codec) {
                            return Err(Error::Unsupported(
                                "mpeg-ts passthrough for this codec is not enabled in this build",
                            ));
                        }
                        context.validate(codec.name())?;
                    }
                }
            }
        }
        self.license = license;
        Ok(())
    }

    /// Returns the next packet in file order, or `None` at end of
    /// stream. Licensed packets are gated: without a feature or
    /// without a license this returns `Unsupported` or
    /// `LicenseRequired` instead of the packet, and the position
    /// does not advance, so the caller can set a license and
    /// retry.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the packet's codec needs a
    /// feature that is off, and `LicenseRequired` when the codec
    /// needs a license that was not set.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.position >= self.packets.len() {
            return Ok(None);
        }
        let packet = &self.packets[self.position];
        passthrough_allowed(packet.codec, self.license.is_some())?;
        self.position += 1;
        Ok(Some(packet.clone()))
    }
}

/// Maps a PMT stream type to a library codec, or `None` for a
/// type this library does not demux.
fn codec_for_stream_type(stream_type: u8) -> Option<Codec> {
    match stream_type {
        TS_STREAM_TYPE_AVC => Some(Codec::Avc),
        TS_STREAM_TYPE_HEVC => Some(Codec::Hevc),
        TS_STREAM_TYPE_AAC => Some(Codec::Aac),
        // ISO/IEC 13818-1 Table 2-34: 0x03 is MPEG-1 audio and
        // 0x04 is MPEG-2 audio; both carry MP3 (Layer III) in
        // practice, and ffmpeg's MPEG-TS muxer writes 0x03.
        TS_STREAM_TYPE_MP3 | TS_STREAM_TYPE_MP3_MPEG2 => Some(Codec::Mp3),
        _ => None,
    }
}

/// Parses a 5-byte PTS/DTS field (ISO/IEC 13818-1 section
/// 2.4.3.6) into microseconds (90 kHz clock).
fn parse_pts(bytes: &[u8]) -> u64 {
    let pts: u64 = ((u64::from(bytes[0] >> 1) & 0x07) << 30)
        | (u64::from(bytes[1]) << 22)
        | ((u64::from(bytes[2] >> 1) & 0x7F) << 15)
        | (u64::from(bytes[3]) << 7)
        | (u64::from(bytes[4] >> 1) & 0x7F);
    pts * 1_000_000 / 90_000
}

/// Parses a PES header from the start of `buffer`. Returns the
/// header once enough bytes are present, or `None` when the
/// buffer is still too short for the header length field.
fn parse_pes_header(buffer: &[u8], offset: usize) -> Result<Option<PesHeader>> {
    if buffer.len() < 9 {
        return Ok(None);
    }
    if buffer[0] != 0 || buffer[1] != 0 || buffer[2] != 1 {
        return Err(Error::InvalidData {
            offset,
            detail: "mpeg-ts PES packet does not start with 00 00 01",
        });
    }
    let stream_id = buffer[3];
    // Only audio (0xC0..=0xDF) and video (0xE0..=0xEF) stream
    // ids carry elementary streams this demuxer returns; other
    // ids (private streams, padding) are not decodable here.
    if !(0xC0..=0xEF).contains(&stream_id) {
        return Err(Error::InvalidData {
            offset,
            detail: "mpeg-ts PES stream id is not an audio or video stream",
        });
    }
    let packet_length = u16::from_be_bytes([buffer[4], buffer[5]]);
    let header_data_length = usize::from(buffer[8]);
    let header_len = 9 + header_data_length;
    if buffer.len() < header_len {
        return Ok(None);
    }
    Ok(Some(PesHeader {
        packet_length,
        header_len,
    }))
}

/// Extracts the PTS from a PES header, when present.
fn pes_pts(buffer: &[u8]) -> Option<u64> {
    // PTS/DTS flags are bits 7-6 of byte 7; '10' means PTS only,
    // '11' means PTS then DTS.
    let flags = (buffer[7] >> 6) & 0x03;
    if (flags == 0b10 || flags == 0b11) && buffer.len() >= 14 {
        return Some(parse_pts(&buffer[9..14]));
    }
    None
}

/// Parses one PSI section payload (after the pointer field) into
/// (table id, section bytes without the 4-byte CRC).
fn psi_section(payload: &[u8], offset: usize) -> Result<(u8, &[u8])> {
    if payload.is_empty() {
        return Err(Error::Truncated { needed: 1, had: 0 });
    }
    let pointer = usize::from(payload[0]);
    let section = payload.get(1 + pointer..).ok_or(Error::Truncated {
        needed: 1 + pointer + 3,
        had: payload.len(),
    })?;
    if section.len() < 3 {
        return Err(Error::Truncated {
            needed: 3,
            had: section.len(),
        });
    }
    let section_length = (usize::from(section[1] & 0x0F) << 8) | usize::from(section[2]);
    if section_length < 4 {
        return Err(Error::InvalidData {
            offset,
            detail: "mpeg-ts section length is smaller than its CRC",
        });
    }
    if section.len() < 3 + section_length {
        return Err(Error::Truncated {
            needed: 3 + section_length,
            had: section.len(),
        });
    }
    Ok((section[0], &section[3..3 + section_length - 4]))
}

/// Parses a PAT section body into (program number, PMT PID) pairs.
fn parse_pat(body: &[u8]) -> Result<Vec<(u16, u16)>> {
    if body.len() < 5 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mpeg-ts PAT section is too short",
        });
    }
    let mut programs = Vec::new();
    let mut pos = 5;
    while pos + 4 <= body.len() {
        let program = u16::from_be_bytes([body[pos], body[pos + 1]]);
        let pid = (u16::from(body[pos + 2] & 0x1F) << 8) | u16::from(body[pos + 3]);
        if program != 0 {
            programs.push((program, pid));
        }
        pos += 4;
    }
    Ok(programs)
}

/// Parses a PMT section body into streams. `pcr_pid` is returned
/// for completeness and ignored by the caller.
fn parse_pmt(body: &[u8]) -> Result<Vec<(u8, u16)>> {
    if body.len() < 9 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mpeg-ts PMT section is too short",
        });
    }
    let pcr_pid = (u16::from(body[5] & 0x1F) << 8) | u16::from(body[6]);
    let _ = pcr_pid;
    let program_info_length = (usize::from(body[7] & 0x0F) << 8) | usize::from(body[8]);
    let mut pos = 9 + program_info_length;
    let mut streams = Vec::new();
    while pos + 5 <= body.len() {
        let stream_type = body[pos];
        let pid = (u16::from(body[pos + 1] & 0x1F) << 8) | u16::from(body[pos + 2]);
        let es_info_length = (usize::from(body[pos + 3] & 0x0F) << 8) | usize::from(body[pos + 4]);
        streams.push((stream_type, pid));
        pos += 5 + es_info_length;
    }
    Ok(streams)
}

/// Parses the adaptation field of one TS packet. Returns
/// (payload start index, PCR in microseconds when present,
/// random-access flag).
fn parse_adaptation(packet: &[u8]) -> Result<(usize, Option<u64>, bool)> {
    let afc = (packet[3] >> 4) & 0x03;
    if afc == 1 {
        return Ok((4, None, false));
    }
    let field_len = usize::from(packet[4]);
    if field_len == 0 {
        return Ok((5, None, false));
    }
    if 5 + field_len > TS_PACKET_LEN {
        return Err(Error::InvalidData {
            offset: 4,
            detail: "mpeg-ts adaptation field overruns the packet",
        });
    }
    let flags = packet[5];
    let random_access = flags & 0x40 != 0;
    let pcr = if flags & 0x10 != 0 {
        if field_len < 7 {
            return Err(Error::InvalidData {
                offset: 5,
                detail: "mpeg-ts PCR flag set but adaptation field is too short",
            });
        }
        let base: u64 = (u64::from(packet[6]) << 25)
            | (u64::from(packet[7]) << 17)
            | (u64::from(packet[8]) << 9)
            | (u64::from(packet[9]) << 1)
            | (u64::from(packet[10]) >> 7);
        let ext: u64 = ((u64::from(packet[10]) & 0x01) << 8) | u64::from(packet[11]);
        // 27 MHz clock: PCR = base * 300 + ext.
        Some((base * 300 + ext) / 27)
    } else {
        None
    };
    Ok((5 + field_len, pcr, random_access))
}

/// Emits the PES held in `pes` for `pid` when it is complete.
/// A declared length that was not reached drops the PES and
/// counts it; an unbounded PES (length 0) is emitted as-is.
/// The PES start offset is recorded alongside so the caller can
/// reorder packets into file order.
fn emit_pes(
    pes: &mut PidState,
    pid: u16,
    streams: &[TsStream],
    packets: &mut Vec<Packet>,
    packet_pids: &mut Vec<u16>,
    packet_starts: &mut Vec<usize>,
    stats: &mut TsStreamStats,
) -> Result<()> {
    let Some(header) = &pes.header else {
        if !pes.buffer.is_empty() {
            stats.incomplete_pes_dropped += 1;
        }
        pes.buffer.clear();
        return Ok(());
    };
    let Some(stream) = streams.iter().find(|stream| stream.pid == pid) else {
        pes.buffer.clear();
        pes.header = None;
        return Ok(());
    };
    let Some(codec) = stream.codec else {
        pes.buffer.clear();
        pes.header = None;
        return Ok(());
    };
    if header.packet_length != 0 {
        let total = 6usize + usize::from(header.packet_length);
        if pes.buffer.len() < total {
            stats.truncated_pes_dropped += 1;
            pes.buffer.clear();
            pes.header = None;
            return Ok(());
        }
        pes.buffer.truncate(total);
    }
    let payload = pes.buffer[header.header_len..].to_vec();
    let limit = match codec {
        Codec::Avc | Codec::Hevc => MAX_VIDEO_PACKET_BYTES,
        _ => MAX_AUDIO_PACKET_BYTES,
    };
    if payload.len() > limit {
        return Err(Error::AllocationLimit {
            requested: payload.len(),
            limit,
        });
    }
    let timestamp = pes.pts_us.unwrap_or(0);
    let is_keyframe = match codec {
        Codec::Avc | Codec::Hevc => pes.random_access,
        _ => true,
    };
    packets.push(Packet {
        codec,
        data: payload,
        timestamp,
        is_keyframe,
    });
    packet_pids.push(pid);
    packet_starts.push(pes.pes_start);
    pes.buffer.clear();
    pes.header = None;
    Ok(())
}

/// Parses a whole MPEG-TS byte string into a `TsDemuxer`.
#[allow(clippy::too_many_lines)]
fn parse_ts(bytes: &[u8]) -> Result<TsDemuxer> {
    if bytes.len() > MAX_MPEGTS_FILE_BYTES {
        return Err(Error::AllocationLimit {
            requested: bytes.len(),
            limit: MAX_MPEGTS_FILE_BYTES,
        });
    }
    if bytes.is_empty() {
        return Err(Error::Truncated {
            needed: TS_PACKET_LEN,
            had: 0,
        });
    }
    if bytes.len() % TS_PACKET_LEN != 0 {
        return Err(Error::Truncated {
            needed: bytes.len() + (TS_PACKET_LEN - bytes.len() % TS_PACKET_LEN),
            had: bytes.len(),
        });
    }
    // Resynchronize: the first packet must start with the sync
    // byte, and the next packet boundary must agree.
    let mut start = 0usize;
    while start < bytes.len() {
        if bytes[start] == TS_SYNC_BYTE
            && (start + TS_PACKET_LEN >= bytes.len()
                || bytes[start + TS_PACKET_LEN] == TS_SYNC_BYTE)
        {
            break;
        }
        start += 1;
    }
    if start >= bytes.len() {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mpeg-ts sync byte not found",
        });
    }
    let mut streams: Vec<TsStream> = Vec::new();
    let mut pmt_pids: Vec<u16> = Vec::new();
    let mut pid_states: HashMap<u16, PidState> = HashMap::new();
    let mut packets: Vec<Packet> = Vec::new();
    let mut packet_pids: Vec<u16> = Vec::new();
    let mut packet_starts: Vec<usize> = Vec::new();
    let mut stats = TsStreamStats::default();
    let mut offset = start;
    while offset + TS_PACKET_LEN <= bytes.len() {
        let packet = &bytes[offset..offset + TS_PACKET_LEN];
        stats.packets += 1;
        if packet[0] != TS_SYNC_BYTE {
            return Err(Error::InvalidData {
                offset,
                detail: "mpeg-ts packet sync byte is not 0x47",
            });
        }
        let tei = packet[1] & 0x80 != 0;
        let pusi = packet[1] & 0x40 != 0;
        let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
        let scrambling = (packet[3] >> 6) & 0x03;
        let afc = (packet[3] >> 4) & 0x03;
        let cc = packet[3] & 0x0F;
        if tei {
            offset += TS_PACKET_LEN;
            continue;
        }
        if scrambling != 0 {
            return Err(Error::InvalidData {
                offset,
                detail: "mpeg-ts packet is scrambled",
            });
        }
        let (payload_start, pcr, random_access) = parse_adaptation(packet)?;
        let payload = &packet[payload_start..];
        // Continuity: only packets with a payload advance the
        // counter; a repeated counter is a duplicate, a skipped
        // counter is a gap that drops the partial PES.
        if afc == 1 || afc == 3 {
            let pes = pid_states.entry(pid).or_default();
            match pes.last_cc {
                Some(last) if cc == last => {
                    stats.dropped_duplicates += 1;
                    offset += TS_PACKET_LEN;
                    continue;
                }
                Some(last) if cc != (last + 1) % 16 => {
                    stats.continuity_errors += 1;
                    if !pes.buffer.is_empty() {
                        stats.incomplete_pes_dropped += 1;
                    }
                    pes.buffer.clear();
                    pes.header = None;
                }
                _ => {}
            }
            pes.last_cc = Some(cc);
        }
        if let Some(pcr_us) = pcr {
            if let Some(pes) = pid_states.get_mut(&pid) {
                pes.last_pcr_us = Some(pcr_us);
            } else {
                pid_states.insert(
                    pid,
                    PidState {
                        last_pcr_us: Some(pcr_us),
                        ..PidState::default()
                    },
                );
            }
            if let Some(stream) = streams.iter_mut().find(|stream| stream.pid == pid) {
                stream.last_pcr_us = Some(pcr_us);
            }
        }
        if pid == TS_PAT_PID {
            if pusi {
                let (table_id, body) = psi_section(payload, offset)?;
                if table_id != 0x00 {
                    return Err(Error::InvalidData {
                        offset,
                        detail: "mpeg-ts PAT table id is not 0",
                    });
                }
                for (_, pmt_pid) in parse_pat(body)? {
                    if !pmt_pids.contains(&pmt_pid) {
                        pmt_pids.push(pmt_pid);
                    }
                }
            }
        } else if pmt_pids.contains(&pid) {
            if pusi {
                let (table_id, body) = psi_section(payload, offset)?;
                if table_id != 0x02 {
                    return Err(Error::InvalidData {
                        offset,
                        detail: "mpeg-ts PMT table id is not 2",
                    });
                }
                for (stream_type, stream_pid) in parse_pmt(body)? {
                    if streams.iter().any(|stream| stream.pid == stream_pid) {
                        continue;
                    }
                    streams.push(TsStream {
                        pid: stream_pid,
                        stream_type,
                        codec: codec_for_stream_type(stream_type),
                        last_pcr_us: None,
                    });
                }
            }
        } else if (afc == 1 || afc == 3) && streams.iter().any(|stream| stream.pid == pid) {
            // Elementary stream payload. PIDs the PMT never
            // declared (SDT, NIT, and other tables) are skipped:
            // their payloads are not PES packets.
            let pes = pid_states.entry(pid).or_default();
            if pusi {
                // A new PES starts: emit the previous one.
                emit_pes(
                    pes,
                    pid,
                    &streams,
                    &mut packets,
                    &mut packet_pids,
                    &mut packet_starts,
                    &mut stats,
                )?;
                pes.random_access = random_access;
                pes.pts_us = None;
                pes.pes_start = offset;
            }
            if pusi || !pes.buffer.is_empty() || pes.header.is_some() {
                pes.buffer.extend_from_slice(payload);
            }
            if pes.header.is_none() {
                if let Some(header) = parse_pes_header(&pes.buffer, offset)? {
                    pes.pts_us = pes_pts(&pes.buffer);
                    pes.header = Some(header);
                }
            }
            // Emit as soon as the declared length is reached.
            if let Some(header) = &pes.header {
                if header.packet_length != 0
                    && pes.buffer.len() >= 6 + usize::from(header.packet_length)
                {
                    emit_pes(
                        pes,
                        pid,
                        &streams,
                        &mut packets,
                        &mut packet_pids,
                        &mut packet_starts,
                        &mut stats,
                    )?;
                }
            }
        }
        offset += TS_PACKET_LEN;
    }
    // End of stream: flush every partial PES.
    let mut pids: Vec<u16> = pid_states.keys().copied().collect();
    pids.sort_unstable();
    for pid in pids {
        if let Some(mut pes) = pid_states.remove(&pid) {
            // Only emit when the PES was actually started; a PID
            // that only carried PSI or nothing has an empty buffer.
            if !pes.buffer.is_empty() {
                emit_pes(
                    &mut pes,
                    pid,
                    &streams,
                    &mut packets,
                    &mut packet_pids,
                    &mut packet_starts,
                    &mut stats,
                )?;
            }
        }
    }
    if pmt_pids.is_empty() {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mpeg-ts stream has no PAT/PMT",
        });
    }
    if packets.is_empty() {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mpeg-ts stream contains no packets",
        });
    }
    // Reorder into file order by PES start offset (see the
    // `pes_start` note on `PidState`).
    let mut order: Vec<usize> = (0..packets.len()).collect();
    order.sort_by_key(|&index| packet_starts[index]);
    let packets: Vec<Packet> = order.iter().map(|&index| packets[index].clone()).collect();
    let packet_pids: Vec<u16> = order.iter().map(|&index| packet_pids[index]).collect();
    Ok(TsDemuxer {
        streams,
        packets,
        packet_pids,
        position: 0,
        stats,
        license: None,
    })
}

/// Muxer for MPEG-TS: writes PAT/PMT once, then one PES per
/// packet, split across 188-byte packets.
#[derive(Debug)]
pub struct TsMuxer {
    program_number: u16,
    pmt_pid: u16,
    streams: Vec<(u16, Codec, u8)>,
    continuity: HashMap<u16, u8>,
    header_written: bool,
}

impl Default for TsMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl TsMuxer {
    /// Creates a muxer: program 1, PMT on PID `0x100`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            program_number: 1,
            pmt_pid: 0x100,
            streams: Vec::new(),
            continuity: HashMap::new(),
            header_written: false,
        }
    }

    /// Registers an elementary stream on `pid`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a reserved PID (0, the PMT
    /// PID, or above `0x1FFE`) or a duplicate PID, and
    /// `Unsupported` for a codec with no TS stream type.
    pub fn add_stream(&mut self, pid: u16, codec: Codec) -> Result<()> {
        if pid == 0 || pid == self.pmt_pid || pid > 0x1FFE {
            return Err(Error::InvalidArgument(
                "mpeg-ts pid is reserved or out of range",
            ));
        }
        if self
            .streams
            .iter()
            .any(|(stream_pid, _, _)| *stream_pid == pid)
        {
            return Err(Error::InvalidArgument("mpeg-ts pid is already registered"));
        }
        let stream_type = match codec {
            Codec::Avc => TS_STREAM_TYPE_AVC,
            Codec::Hevc => TS_STREAM_TYPE_HEVC,
            Codec::Aac => TS_STREAM_TYPE_AAC,
            // MP3 muxes as MPEG-1 audio (0x03), matching what
            // ffmpeg's MPEG-TS muxer writes for MP3.
            Codec::Mp3 => TS_STREAM_TYPE_MP3,
            _ => {
                return Err(Error::Unsupported(
                    "mpeg-ts muxing for this codec is not supported",
                ));
            }
        };
        self.streams.push((pid, codec, stream_type));
        Ok(())
    }

    /// PAT + PMT bytes for this muxer (two TS packets).
    #[must_use]
    pub fn pat_pmt(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.pat_packet());
        out.extend_from_slice(&self.pmt_packet());
        out
    }

    /// Builds one PAT packet: program 1 on the PMT PID.
    fn pat_packet(&self) -> [u8; TS_PACKET_LEN] {
        let mut section: Vec<u8> = Vec::new();
        section.push(0x00);
        section.push(0xB0);
        section.push(0x0D);
        section.extend_from_slice(&1u16.to_be_bytes());
        section.push(0xC1);
        section.push(0x00);
        section.push(0x00);
        section.extend_from_slice(&self.program_number.to_be_bytes());
        section.push(0xE0 | u8::try_from(self.pmt_pid >> 8).unwrap_or(0));
        section.push(u8::try_from(self.pmt_pid & 0xFF).unwrap_or(0));
        let crc = crc32_mpeg(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        let mut payload: Vec<u8> = Vec::new();
        payload.push(0);
        payload.extend_from_slice(&section);
        self.ts_packet(TS_PAT_PID, true, &payload, None, false)
    }

    /// Builds one PMT packet for the registered streams.
    fn pmt_packet(&self) -> [u8; TS_PACKET_LEN] {
        let pcr_pid = self.streams.first().map_or(0x101, |(pid, _, _)| *pid);
        let section_len = 9 + 5 * self.streams.len() + 4;
        let mut section: Vec<u8> = Vec::new();
        section.push(0x02);
        section.push(0xB0 | u8::try_from((section_len >> 8) & 0x0F).unwrap_or(0));
        section.push(u8::try_from(section_len & 0xFF).unwrap_or(0));
        section.extend_from_slice(&self.program_number.to_be_bytes());
        section.push(0xC1);
        section.push(0x00);
        section.push(0x00);
        section.push(0xE0 | u8::try_from(pcr_pid >> 8).unwrap_or(0));
        section.push(u8::try_from(pcr_pid & 0xFF).unwrap_or(0));
        section.push(0xF0);
        section.push(0x00);
        for (pid, _, stream_type) in &self.streams {
            section.push(*stream_type);
            section.push(0xE0 | u8::try_from(pid >> 8).unwrap_or(0));
            section.push(u8::try_from(pid & 0xFF).unwrap_or(0));
            section.push(0xF0);
            section.push(0x00);
        }
        let crc = crc32_mpeg(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        let mut payload: Vec<u8> = Vec::new();
        payload.push(0);
        payload.extend_from_slice(&section);
        self.ts_packet(self.pmt_pid, true, &payload, None, false)
    }

    /// Builds one 188-byte TS packet. `payload` must fit the
    /// space left after the header and the optional PCR
    /// adaptation field; the caller pads short payloads by
    /// passing a longer slice or by splitting.
    fn ts_packet(
        &self,
        pid: u16,
        pusi: bool,
        payload: &[u8],
        pcr_us: Option<u64>,
        random_access: bool,
    ) -> [u8; TS_PACKET_LEN] {
        // The continuity counter is taken from a copy of the map:
        // this helper is used for PAT/PMT, whose counters never
        // advance past 0 in practice.
        let cc = self.continuity.get(&pid).copied().unwrap_or(0);
        ts_packet_bytes(pid, pusi, cc, payload, pcr_us, random_access)
    }

    /// Muxes one PES packet: `payload` is the raw codec data
    /// (Annex B for AVC/HEVC, ADTS frames for AAC), `pts_us` the
    /// presentation timestamp. The PAT/PMT is prepended on the
    /// first call. Returns the TS bytes (a multiple of 188).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for an unregistered PID and
    /// `AllocationLimit` when the payload exceeds the per-codec
    /// packet limit.
    pub fn packet(
        &mut self,
        pid: u16,
        payload: &[u8],
        pts_us: Option<u64>,
        random_access: bool,
    ) -> Result<Vec<u8>> {
        let codec = self
            .streams
            .iter()
            .find(|(stream_pid, _, _)| *stream_pid == pid)
            .map(|(_, codec, _)| *codec)
            .ok_or(Error::InvalidArgument(
                "mpeg-ts pid is not registered with the muxer",
            ))?;
        let limit = match codec {
            Codec::Avc | Codec::Hevc => MAX_VIDEO_PACKET_BYTES,
            _ => MAX_AUDIO_PACKET_BYTES,
        };
        if payload.len() > limit {
            return Err(Error::AllocationLimit {
                requested: payload.len(),
                limit,
            });
        }
        let mut out: Vec<u8> = Vec::new();
        if !self.header_written {
            out.extend_from_slice(&self.pat_pmt());
            self.header_written = true;
        }
        let pes = build_pes(codec, payload, pts_us);
        // Split the PES across TS packets; the first carries the
        // PCR when a timestamp is present.
        let mut offset = 0usize;
        let mut first = true;
        while offset < pes.len() {
            let pcr = if first { pts_us } else { None };
            let header_len = if pcr.is_some() { 4 + 8 } else { 4 };
            let capacity = TS_PACKET_LEN - header_len;
            let take = (pes.len() - offset).min(capacity);
            let chunk = &pes[offset..offset + take];
            let cc = self.continuity.entry(pid).or_insert(0);
            let packet = ts_packet_bytes(pid, first, *cc, chunk, pcr, random_access && first);
            *cc = (*cc + 1) % 16;
            out.extend_from_slice(&packet);
            offset += take;
            first = false;
        }
        Ok(out)
    }
}

/// Builds one TS packet with stuffing: `payload` is shorter than
/// the available space, so an adaptation field pads the rest.
fn ts_packet_bytes(
    pid: u16,
    pusi: bool,
    cc: u8,
    payload: &[u8],
    pcr_us: Option<u64>,
    random_access: bool,
) -> [u8; TS_PACKET_LEN] {
    let mut out = [0xFFu8; TS_PACKET_LEN];
    out[0] = TS_SYNC_BYTE;
    out[1] = (if pusi { 0x40 } else { 0x00 }) | u8::try_from(pid >> 8).unwrap_or(0);
    out[2] = u8::try_from(pid & 0xFF).unwrap_or(0);
    // Space for the payload after the 4-byte header and the
    // adaptation field (1 length byte + pcr bytes, plus stuffing
    // to fill the packet).
    let available = TS_PACKET_LEN - 4;
    let payload_len = payload.len().min(available);
    let adaptation_len = available - payload_len;
    if adaptation_len == 0 {
        out[3] = 0x10 | (cc & 0x0F);
        out[4..4 + payload_len].copy_from_slice(&payload[..payload_len]);
        return out;
    }
    // adaptation_field_control = 3 (adaptation + payload).
    out[3] = 0x30 | (cc & 0x0F);
    // adaptation_field_length counts the bytes after itself.
    let field_len = adaptation_len - 1;
    out[4] = u8::try_from(field_len).unwrap_or(0);
    let mut pos = 5usize;
    if let Some(pcr) = pcr_us {
        out[pos] = 0x10 | if random_access { 0x40 } else { 0x00 };
        pos += 1;
        // PCR: 33-bit base at 90 kHz, 9-bit extension at 27 MHz.
        let base = pcr * 90_000 / 1_000_000;
        let ext = (pcr * 27) % 300;
        let base = base & 0x1_FFFF_FFFF;
        out[pos] = u8::try_from((base >> 25) & 0xFF).unwrap_or(0);
        out[pos + 1] = u8::try_from((base >> 17) & 0xFF).unwrap_or(0);
        out[pos + 2] = u8::try_from((base >> 9) & 0xFF).unwrap_or(0);
        out[pos + 3] = u8::try_from((base >> 1) & 0xFF).unwrap_or(0);
        out[pos + 4] = u8::try_from(((base & 0x01) << 7) | 0x7E | ((ext >> 8) & 0x01)).unwrap_or(0);
        out[pos + 5] = u8::try_from(ext & 0xFF).unwrap_or(0);
        pos += 6;
    } else if field_len > 0 {
        out[pos] = if random_access { 0x40 } else { 0x00 };
        pos += 1;
    }
    // The rest of the adaptation field is stuffing (0xFF, already
    // the fill value); the payload follows it.
    let payload_start = 5 + field_len;
    out[payload_start..payload_start + payload_len].copy_from_slice(&payload[..payload_len]);
    debug_assert!(pos <= payload_start);
    out
}

/// Builds one PES packet (ISO/IEC 13818-1 section 2.4.3.6).
fn build_pes(codec: Codec, payload: &[u8], pts_us: Option<u64>) -> Vec<u8> {
    let stream_id: u8 = match codec {
        Codec::Avc | Codec::Hevc => 0xE0,
        _ => 0xC0,
    };
    let mut pes: Vec<u8> = Vec::new();
    pes.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
    let (flags, header_data_len, pts_bytes) = match pts_us {
        Some(pts) => {
            let pts_90k = pts * 90_000 / 1_000_000;
            (0x80u8, 5usize, encode_pts(pts_90k))
        }
        None => (0x00u8, 0usize, [0u8; 5]),
    };
    // PES_packet_length counts the 3 flag bytes, the PTS bytes,
    // and the payload; video uses 0 (unbounded).
    let body_len = 3 + header_data_len + payload.len();
    let pes_packet_length: u16 = match codec {
        Codec::Avc | Codec::Hevc => 0,
        _ => {
            if body_len <= 0xFFFF {
                u16::try_from(body_len).unwrap_or(0)
            } else {
                0
            }
        }
    };
    pes.extend_from_slice(&pes_packet_length.to_be_bytes());
    pes.push(0x80);
    pes.push(flags);
    pes.push(u8::try_from(header_data_len).unwrap_or(0));
    if pts_us.is_some() {
        pes.extend_from_slice(&pts_bytes);
    }
    pes.extend_from_slice(payload);
    pes
}

/// Encodes a 33-bit PTS value (90 kHz) into 5 bytes.
fn encode_pts(pts: u64) -> [u8; 5] {
    let pts = pts & 0x1_FFFF_FFFF;
    let part = |shift: u32, mask: u64| u8::try_from((pts >> shift) & mask).unwrap_or(0);
    [
        0x20 | (part(30, 0x07) << 1) | 0x01,
        part(22, 0xFF),
        (part(15, 0x7F) << 1) | 0x01,
        part(7, 0xFF),
        (part(0, 0x7F) << 1) | 0x01,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_mpeg_check_value() {
        assert_eq!(crc32_mpeg(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn pts_round_trip() {
        let pts = parse_pts(&encode_pts(90_000));
        assert_eq!(pts, 1_000_000);
    }

    #[test]
    fn mux_demux_round_trip() {
        let mut muxer = TsMuxer::new();
        muxer.add_stream(0x101, Codec::Avc).unwrap();
        muxer.add_stream(0x102, Codec::Aac).unwrap();
        let video = vec![0x00u8, 0x00, 0x00, 0x01, 0x65, 0x11, 0x22];
        let audio = vec![0xFFu8, 0xF1, 0x50, 0x40, 0x14, 0x1F, 0xFC, 0xAA, 0xBB];
        let mut bytes = muxer.packet(0x101, &video, Some(0), true).unwrap();
        bytes.extend_from_slice(&muxer.packet(0x102, &audio, Some(0), false).unwrap());
        assert_eq!(bytes.len() % TS_PACKET_LEN, 0);
        let demuxer = TsDemuxer::parse(&bytes).unwrap();
        assert_eq!(demuxer.streams().len(), 2);
        assert_eq!(demuxer.streams()[0].codec, Some(Codec::Avc));
        assert_eq!(demuxer.streams()[1].codec, Some(Codec::Aac));
        assert_eq!(demuxer.packet_count(), 2);
        assert_eq!(demuxer.stats().continuity_errors, 0);
    }

    #[test]
    fn mp3_mux_demux_round_trip_needs_no_license() {
        let mut muxer = TsMuxer::new();
        muxer.add_stream(0x101, Codec::Mp3).unwrap();
        // A minimal MP3 frame header (MPEG-1 Layer III, 128 kbps,
        // 44100 Hz) followed by filler: the TS layer only carries
        // the bytes, so the payload is checked verbatim.
        let mut frame = vec![0xFFu8, 0xFB, 0x90, 0x00];
        frame.extend(std::iter::repeat_n(0u8, 413));
        let bytes = muxer.packet(0x101, &frame, Some(0), false).unwrap();
        assert_eq!(bytes.len() % TS_PACKET_LEN, 0);
        let mut demuxer = TsDemuxer::parse(&bytes).unwrap();
        assert_eq!(demuxer.streams().len(), 1);
        assert_eq!(demuxer.streams()[0].stream_type, TS_STREAM_TYPE_MP3);
        assert_eq!(demuxer.streams()[0].codec, Some(Codec::Mp3));
        assert_eq!(demuxer.packet_count(), 1);
        let packet = demuxer.next_packet().unwrap().unwrap();
        assert_eq!(packet.codec, Codec::Mp3);
        assert_eq!(packet.data, frame);
    }

    #[test]
    fn empty_input_is_truncated() {
        let err = TsDemuxer::parse(&[]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn non_multiple_of_188_is_truncated() {
        let err = TsDemuxer::parse(&[0x47u8; 100]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn no_sync_is_invalid_data() {
        let err = TsDemuxer::parse(&[0u8; 188]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn no_pat_is_invalid_data() {
        // One sync-correct packet on a PID that is neither PAT
        // nor PMT: the stream has no tables.
        let packet = ts_packet_bytes(0x101, false, 0, &[], None, false);
        let err = TsDemuxer::parse(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn duplicate_pid_is_invalid_argument() {
        let mut muxer = TsMuxer::new();
        muxer.add_stream(0x101, Codec::Avc).unwrap();
        let err = muxer.add_stream(0x101, Codec::Aac).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn unregistered_pid_is_invalid_argument() {
        let mut muxer = TsMuxer::new();
        muxer.add_stream(0x101, Codec::Avc).unwrap();
        let err = muxer.packet(0x102, &[0u8; 4], None, false).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}

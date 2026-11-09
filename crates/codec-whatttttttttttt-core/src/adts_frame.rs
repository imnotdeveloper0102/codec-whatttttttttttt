//! ADTS framing for AAC (ISO/IEC 13818-7).
//!
//! An ADTS stream is a sequence of frames, each with a 7-byte header
//! (9 bytes when error protection is present) followed by one AAC raw
//! data block. This module only frames: it never decodes AAC samples.
//! The header gives the profile, sample rate, channel configuration,
//! and frame length, so a caller can split a stream into packets and
//! hand each raw block to a decoder.
//!
//! CRC (ISO/IEC 13818-7, reusing the CRC-16 of ISO/IEC 11172-3
//! section 2.4.3.1): polynomial `0x8005`, initial value `0xFFFF`,
//! MSB-first, computed over the 7 header bytes and the raw data
//! block, excluding the 2-byte `crc_check` field itself. A mismatch
//! is `InvalidData`.

use crate::error::{Error, Result};
use crate::limits::{MAX_ADTS_FILE_BYTES, MAX_AUDIO_PACKET_BYTES};

/// ADTS sync word, top 12 bits of the first two bytes (`0xFFF`).
pub const ADTS_SYNC: u16 = 0xFFF;

/// Header length without CRC, in bytes (ISO/IEC 13818-7).
pub const ADTS_HEADER_LEN: usize = 7;

/// Header length with CRC, in bytes.
pub const ADTS_HEADER_LEN_CRC: usize = 9;

/// Sample rates indexed by `sampling_frequency_index` (ISO/IEC
/// 13818-7 Table 1.16). Indices 13 (reserved) and 14 (escape) have
/// no rate here; index 15 is forbidden.
pub const ADTS_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// One parsed ADTS frame header (ISO/IEC 13818-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
    /// AAC profile: 0 Main, 1 LC, 2 SSR.
    pub profile: u8,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// `sampling_frequency_index` from the header.
    pub sampling_frequency_index: u8,
    /// Channel configuration (1..=7; 0 needs a PCE and is rejected).
    pub channels: u8,
    /// Total frame length in bytes, header included.
    pub frame_length: usize,
    /// True when no CRC follows the header.
    pub protection_absent: bool,
    /// CRC value when present.
    pub crc: Option<u16>,
    /// Number of raw data blocks minus one (0 for one block).
    pub raw_data_blocks: u8,
    /// Buffer fullness in 32-bit words (`0x7FF` means VBR).
    pub buffer_fullness: u16,
}

/// Looks up a sample rate by `sampling_frequency_index`.
///
/// # Errors
///
/// Returns `InvalidData` for index 13 (reserved), 14 (escape, the
/// rate lives in an explicit field this parser does not read), and
/// 15 (forbidden).
pub fn sample_rate_for_index(index: u8) -> Result<u32> {
    ADTS_SAMPLE_RATES
        .get(usize::from(index))
        .copied()
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "adts sampling frequency index 13, 14, or 15 has no table rate",
        })
}

/// CRC-16 for ADTS error protection (ISO/IEC 11172-3 section
/// 2.4.3.1): polynomial `0x8005`, initial value `0xFFFF`,
/// MSB-first, no final xor. Check value: `0xAEE7` for the ASCII
/// string `"123456789"`.
#[must_use]
pub fn adts_crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x8005;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Parses one ADTS header at the start of `data`. Returns the header
/// and the header length in bytes (7 or 9).
///
/// # Errors
///
/// Returns `Truncated` when fewer than 7 bytes are present or the
/// header claims a CRC that is not fully present, `InvalidData`
/// when the sync word, layer, profile, sample-rate index, channel
/// configuration, or frame length is invalid, and
/// `AllocationLimit` when the frame length exceeds the audio
/// packet limit.
pub fn parse_adts_header(data: &[u8]) -> Result<(AdtsHeader, usize)> {
    if data.len() < ADTS_HEADER_LEN {
        return Err(Error::Truncated {
            needed: ADTS_HEADER_LEN,
            had: data.len(),
        });
    }
    if data[0] != 0xFF || data[1] & 0xF0 != 0xF0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "adts sync word is not 0xFFF",
        });
    }
    // Byte 1 low nibble: ID(1), layer(2), protection_absent(1).
    // Layer must be 0 (ISO/IEC 13818-7).
    if data[1] & 0x06 != 0 {
        return Err(Error::InvalidData {
            offset: 1,
            detail: "adts layer must be 0",
        });
    }
    let protection_absent = data[1] & 0x01 != 0;
    let profile = (data[2] >> 6) & 0x03;
    if profile == 3 {
        return Err(Error::InvalidData {
            offset: 2,
            detail: "adts profile 3 is reserved",
        });
    }
    let sampling_frequency_index = (data[2] >> 2) & 0x0F;
    let sample_rate = sample_rate_for_index(sampling_frequency_index)?;
    let channels = ((data[2] & 0x01) << 2) | (data[3] >> 6);
    if channels == 0 {
        return Err(Error::InvalidData {
            offset: 2,
            detail: "adts channel configuration 0 needs a program config element",
        });
    }
    let frame_length =
        usize::from(data[3] & 0x03) << 11 | usize::from(data[4]) << 3 | usize::from(data[5]) >> 5;
    let header_len = if protection_absent {
        ADTS_HEADER_LEN
    } else {
        ADTS_HEADER_LEN_CRC
    };
    if frame_length < header_len {
        return Err(Error::InvalidData {
            offset: 3,
            detail: "adts frame length is smaller than its header",
        });
    }
    if frame_length > MAX_AUDIO_PACKET_BYTES {
        return Err(Error::AllocationLimit {
            requested: frame_length,
            limit: MAX_AUDIO_PACKET_BYTES,
        });
    }
    let buffer_fullness = (u16::from(data[5]) & 0x1F) << 6 | u16::from(data[6]) >> 2;
    let raw_data_blocks = data[6] & 0x03;
    let crc = if protection_absent {
        None
    } else {
        if data.len() < ADTS_HEADER_LEN_CRC {
            return Err(Error::Truncated {
                needed: ADTS_HEADER_LEN_CRC,
                had: data.len(),
            });
        }
        Some(u16::from_be_bytes([data[7], data[8]]))
    };
    Ok((
        AdtsHeader {
            profile,
            sample_rate,
            sampling_frequency_index,
            channels,
            frame_length,
            protection_absent,
            crc,
            raw_data_blocks,
            buffer_fullness,
        },
        header_len,
    ))
}

/// Verifies one complete ADTS frame: header, full frame present,
/// and CRC when present. Returns the parsed header.
///
/// # Errors
///
/// Returns `Truncated` when the frame is cut short, `InvalidData`
/// for a bad header or a CRC mismatch, and `AllocationLimit` when
/// the frame length exceeds the audio packet limit.
pub fn verify_adts_frame(data: &[u8]) -> Result<AdtsHeader> {
    let (header, header_len) = parse_adts_header(data)?;
    if data.len() < header.frame_length {
        return Err(Error::Truncated {
            needed: header.frame_length,
            had: data.len(),
        });
    }
    if let Some(expected) = header.crc {
        // CRC over the 7 header bytes plus the raw data block.
        let mut crc_input: Vec<u8> = Vec::with_capacity(header.frame_length - 2);
        crc_input.extend_from_slice(&data[..ADTS_HEADER_LEN]);
        crc_input.extend_from_slice(&data[header_len..header.frame_length]);
        if adts_crc16(&crc_input) != expected {
            return Err(Error::InvalidData {
                offset: 7,
                detail: "adts crc mismatch",
            });
        }
    }
    Ok(header)
}

/// One ADTS frame: its header, the raw AAC data block, and its
/// byte offset in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtsFrame {
    /// Parsed frame header.
    pub header: AdtsHeader,
    /// Raw AAC data block (header and CRC stripped).
    pub payload: Vec<u8>,
    /// Byte offset of the frame in the source stream.
    pub offset: usize,
}

/// Demuxer for an ADTS stream held in memory. Frames are parsed
/// on demand; a truncated tail or a CRC mismatch is reported by
/// `next_frame` instead of silently dropping the frame.
#[derive(Debug)]
pub struct AdtsDemuxer {
    data: Vec<u8>,
    position: usize,
}

impl AdtsDemuxer {
    /// Parses an already-read ADTS stream. The stream must start
    /// with an ADTS sync word.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for an empty stream, `InvalidData`
    /// when the first bytes are not an ADTS header, and
    /// `AllocationLimit` when the stream exceeds the ADTS file
    /// limit.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() > MAX_ADTS_FILE_BYTES {
            return Err(Error::AllocationLimit {
                requested: data.len(),
                limit: MAX_ADTS_FILE_BYTES,
            });
        }
        if data.is_empty() {
            return Err(Error::Truncated {
                needed: ADTS_HEADER_LEN,
                had: 0,
            });
        }
        parse_adts_header(data)?;
        Ok(Self {
            data: data.to_vec(),
            position: 0,
        })
    }

    /// Byte offset of the next frame.
    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }

    /// Returns the next frame, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short frame, `InvalidData`
    /// for a bad header or a CRC mismatch, and `AllocationLimit`
    /// for a frame above the audio packet limit.
    pub fn next_frame(&mut self) -> Result<Option<AdtsFrame>> {
        if self.position >= self.data.len() {
            return Ok(None);
        }
        let offset = self.position;
        let remaining = &self.data[offset..];
        let header = verify_adts_frame(remaining)?;
        let (_, header_len) = parse_adts_header(remaining)?;
        let payload = remaining[header_len..header.frame_length].to_vec();
        self.position += header.frame_length;
        Ok(Some(AdtsFrame {
            header,
            payload,
            offset,
        }))
    }
}

/// Writes one ADTS frame around a raw AAC data block.
///
/// # Errors
///
/// Returns `InvalidArgument` for a reserved profile, an unknown
/// sample rate, a channel count outside 1..=7, or a payload that
/// makes the frame exceed the audio packet limit.
pub fn write_adts_frame(
    payload: &[u8],
    profile: u8,
    sample_rate: u32,
    channels: u8,
    with_crc: bool,
) -> Result<Vec<u8>> {
    if profile > 2 {
        return Err(Error::InvalidArgument(
            "adts profile must be 0 (Main), 1 (LC), or 2 (SSR)",
        ));
    }
    if !(1..=7).contains(&channels) {
        return Err(Error::InvalidArgument(
            "adts channel configuration must be 1..=7",
        ));
    }
    let sampling_frequency_index = u8::try_from(
        ADTS_SAMPLE_RATES
            .iter()
            .position(|&rate| rate == sample_rate)
            .ok_or(Error::InvalidArgument(
                "adts sample rate is not in the sampling frequency table",
            ))?,
    )
    .map_err(|_| Error::InvalidArgument("adts sampling frequency index exceeds u8"))?;
    let header_len = if with_crc {
        ADTS_HEADER_LEN_CRC
    } else {
        ADTS_HEADER_LEN
    };
    let frame_length = header_len
        .checked_add(payload.len())
        .ok_or(Error::Overflow("adts frame length overflows"))?;
    if frame_length > MAX_AUDIO_PACKET_BYTES {
        return Err(Error::AllocationLimit {
            requested: frame_length,
            limit: MAX_AUDIO_PACKET_BYTES,
        });
    }
    let mut out: Vec<u8> = Vec::with_capacity(frame_length);
    out.push(0xFF);
    out.push(if with_crc { 0xF0 } else { 0xF1 });
    out.push((profile << 6) | (sampling_frequency_index << 2) | ((channels >> 2) & 0x01));
    out.push(((channels & 0x03) << 6) | u8::try_from((frame_length >> 11) & 0x03).unwrap_or(0));
    out.push(u8::try_from((frame_length >> 3) & 0xFF).unwrap_or(0));
    // Buffer fullness 0x7FF (VBR) in the top 5 bits; the low 3
    // bits are the top of the frame length.
    out.push(u8::try_from(((frame_length & 0x07) << 5) | 0x1F).unwrap_or(0));
    out.push(0xFC);
    if with_crc {
        out.push(0);
        out.push(0);
        // CRC over the 7 header bytes plus the payload; the CRC
        // field itself is excluded.
        let mut crc_input: Vec<u8> = Vec::with_capacity(ADTS_HEADER_LEN + payload.len());
        crc_input.extend_from_slice(&out[..ADTS_HEADER_LEN]);
        crc_input.extend_from_slice(payload);
        let crc = adts_crc16(&crc_input);
        out[ADTS_HEADER_LEN] = u8::try_from(crc >> 8).unwrap_or(0);
        out[ADTS_HEADER_LEN + 1] = u8::try_from(crc & 0xFF).unwrap_or(0);
    }
    out.extend_from_slice(payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_check_value_matches_spec() {
        assert_eq!(adts_crc16(b"123456789"), 0xAEE7);
    }

    #[test]
    fn header_parses_ffmpeg_frame() {
        // First frame of tests/data/sine_aac.adts: fff1 5040 141f fc.
        let data = [0xFF, 0xF1, 0x50, 0x40, 0x14, 0x1F, 0xFC];
        let (header, len) = parse_adts_header(&data).unwrap();
        assert_eq!(len, 7);
        assert_eq!(header.profile, 1);
        assert_eq!(header.sample_rate, 44_100);
        assert_eq!(header.channels, 1);
        assert_eq!(header.frame_length, 160);
        assert!(header.protection_absent);
        assert_eq!(header.crc, None);
    }

    #[test]
    fn bad_sync_is_invalid_data() {
        let err = parse_adts_header(&[0xFF, 0xE0, 0, 0, 0, 0, 0]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn reserved_profile_is_invalid_data() {
        let err = parse_adts_header(&[0xFF, 0xF1, 0xD0, 0x40, 0x14, 0x1F, 0xFC]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn short_header_is_truncated() {
        let err = parse_adts_header(&[0xFF, 0xF1, 0x50]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn write_and_verify_round_trips_with_crc() {
        let payload = vec![0x11u8; 100];
        let frame = write_adts_frame(&payload, 1, 44_100, 2, true).unwrap();
        assert_eq!(frame.len(), 9 + 100);
        let header = verify_adts_frame(&frame).unwrap();
        assert_eq!(header.channels, 2);
        assert_eq!(header.frame_length, 109);
        assert!(header.crc.is_some());
        let mut demuxer = AdtsDemuxer::from_bytes(&frame).unwrap();
        let parsed = demuxer.next_frame().unwrap().unwrap();
        assert_eq!(parsed.payload, payload);
        assert!(demuxer.next_frame().unwrap().is_none());
    }

    #[test]
    fn write_and_verify_round_trips_without_crc() {
        let payload = vec![0x22u8; 50];
        let frame = write_adts_frame(&payload, 1, 48_000, 1, false).unwrap();
        assert_eq!(frame.len(), 7 + 50);
        assert!(verify_adts_frame(&frame).is_ok());
    }

    #[test]
    fn crc_mismatch_is_invalid_data() {
        let payload = vec![0x33u8; 20];
        let mut frame = write_adts_frame(&payload, 1, 44_100, 1, true).unwrap();
        frame[10] ^= 0xFF;
        let err = verify_adts_frame(&frame).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn truncated_frame_is_truncated() {
        let payload = vec![0x44u8; 20];
        let frame = write_adts_frame(&payload, 1, 44_100, 1, false).unwrap();
        let cut = &frame[..frame.len() - 5];
        let err = verify_adts_frame(cut).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn unknown_sample_rate_is_invalid_argument() {
        let err = write_adts_frame(&[0u8; 4], 1, 44_101, 1, false).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}

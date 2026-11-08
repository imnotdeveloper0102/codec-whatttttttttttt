//! FLAC stream and frame decoding (RFC 9639).
//!
//! A FLAC file is the `fLaC` marker, a series of metadata blocks, then
//! frames. Each frame is independent: a frame header (with a CRC-8), one
//! subframe per channel, and a CRC-16 footer. Subframes are constant,
//! verbatim, fixed-predictor, or LPC; the residual in the predicted
//! subframes is partitioned Rice code (RFC 9639 section 9.2.7).
//!
//! All arithmetic on decoded samples is `i64`: Appendix A.3 of the spec
//! shows a prediction can need the sample bit depth plus the predictor
//! order bits (fixed) or plus the coefficient precision and
//! `ceil(log2(order))` (LPC), so `i32` is not wide enough for 32-bit
//! audio. The residual itself is bounded by section 9.2.7.3 to
//! `|value| < 2^31` and is rejected otherwise.
//!
//! `FlacDemuxer` finds frame boundaries by decoding each frame once to
//! learn its length, then `Decoder` decodes it again. That double decode
//! is a known limitation, recorded in `CHANGELOG.md`; a streaming
//! demuxer that computes frame length without a full decode is a later
//! phase.

use std::io::Read;

use crate::bitreader::BitReader;
use crate::bitwriter::BitWriter;
use crate::byte_source::read_flac_bytes;
use crate::error::{Error, Result};
use crate::frame::{AudioFrame, Codec, Metadata, MetadataEntry, Packet};
use crate::limits::{MAX_CHANNELS, MAX_FRAME_SAMPLES, MAX_METADATA_BYTES, MAX_SAMPLE_RATE};

/// CRC-8 with polynomial x^8 + x^2 + x + 1 (0x07), initial value 0
/// (RFC 9639 section 9.1.8). Covers the frame header before the CRC.
#[must_use]
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16 with polynomial x^16 + x^15 + x^2 + x^0 (0x8005), initial
/// value 0 (RFC 9639 section 9.3). Covers the whole frame excluding
/// the CRC itself.
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The 34-byte STREAMINFO metadata block (RFC 9639 section 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    /// Minimum block size in interchannel samples.
    pub min_block_size: u16,
    /// Maximum block size in interchannel samples.
    pub max_block_size: u16,
    /// Minimum frame size in bytes, or 0 when unknown.
    pub min_frame_size: u32,
    /// Maximum frame size in bytes, or 0 when unknown.
    pub max_frame_size: u32,
    /// Sample rate in Hz, or 0 when unknown.
    pub sample_rate: u32,
    /// Number of channels (1..=8).
    pub channels: u32,
    /// Bits per sample (4..=32).
    pub bits_per_sample: u32,
    /// Total number of interchannel samples, or 0 when unknown.
    pub total_samples: u64,
    /// MD5 of the decoded PCM (signed little-endian samples), or all
    /// zeros when unset.
    pub md5: [u8; 16],
}

/// Parses a 34-byte STREAMINFO block.
///
/// # Errors
///
/// Returns `Truncated` when fewer than 34 bytes are given, `InvalidData`
/// for a channel count, bit depth, or block-size range the spec forbids,
/// and `Overflow` for a sample rate above the limit in `limits.rs`.
pub fn parse_streaminfo(data: &[u8]) -> Result<StreamInfo> {
    if data.len() < 34 {
        return Err(Error::Truncated {
            needed: 34,
            had: data.len(),
        });
    }
    let min_block_size = u16::from_be_bytes([data[0], data[1]]);
    let max_block_size = u16::from_be_bytes([data[2], data[3]]);
    let min_frame_size =
        (u32::from(data[4]) << 16) | (u32::from(data[5]) << 8) | u32::from(data[6]);
    let max_frame_size =
        (u32::from(data[7]) << 16) | (u32::from(data[8]) << 8) | u32::from(data[9]);
    let sample_rate =
        (u32::from(data[10]) << 12) | (u32::from(data[11]) << 4) | (u32::from(data[12]) >> 4);
    let channels = u32::from((data[12] >> 1) & 0x07) + 1;
    let bits_per_sample = (u32::from(data[12] & 0x01) << 4) | (u32::from(data[13]) >> 4);
    let bits_per_sample = bits_per_sample + 1;
    let total_samples = (u64::from(data[13] & 0x0F) << 32)
        | (u64::from(data[14]) << 24)
        | (u64::from(data[15]) << 16)
        | (u64::from(data[16]) << 8)
        | u64::from(data[17]);
    let mut md5 = [0u8; 16];
    md5.copy_from_slice(&data[18..34]);
    if min_block_size != 0 && max_block_size != 0 && min_block_size > max_block_size {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "streaminfo minimum block size exceeds maximum",
        });
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidData {
            offset: 12,
            detail: "streaminfo channel count out of range",
        });
    }
    if !(4..=32).contains(&bits_per_sample) {
        return Err(Error::InvalidData {
            offset: 12,
            detail: "streaminfo bits per sample out of range",
        });
    }
    if sample_rate > MAX_SAMPLE_RATE {
        return Err(Error::Overflow("streaminfo sample rate exceeds maximum"));
    }
    Ok(StreamInfo {
        min_block_size,
        max_block_size,
        min_frame_size,
        max_frame_size,
        sample_rate,
        channels,
        bits_per_sample,
        total_samples,
        md5,
    })
}

/// A parsed FLAC stream: streaminfo, container metadata, and the offset
/// of the first audio frame.
#[derive(Debug, Clone)]
pub struct FlacStream {
    /// Audio properties from the STREAMINFO block.
    pub streaminfo: StreamInfo,
    /// Vorbis comment entries, in file order. The vendor string is
    /// stored under the key `VENDOR`.
    pub metadata: Metadata,
    /// Byte offset of the first frame in the source data.
    pub first_frame_offset: usize,
}

fn parse_vorbis_comment(data: &[u8], metadata: &mut Metadata) -> Result<()> {
    let mut offset = 0usize;
    let take_u32 = |data: &[u8], offset: &mut usize| -> Result<u32> {
        let bytes = data.get(*offset..*offset + 4).ok_or(Error::Truncated {
            needed: *offset + 4,
            had: data.len(),
        })?;
        *offset += 4;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    let vendor_len = take_u32(data, &mut offset)? as usize;
    let vendor = data
        .get(offset..offset + vendor_len)
        .ok_or(Error::Truncated {
            needed: offset + vendor_len,
            had: data.len(),
        })?;
    metadata.entries.push(MetadataEntry::new("VENDOR", vendor));
    offset += vendor_len;
    let count = take_u32(data, &mut offset)?;
    for _ in 0..count {
        let entry_len = take_u32(data, &mut offset)? as usize;
        let entry = data
            .get(offset..offset + entry_len)
            .ok_or(Error::Truncated {
                needed: offset + entry_len,
                had: data.len(),
            })?;
        offset += entry_len;
        // A comment is `KEY=value`; without `=` the whole string is
        // kept under an empty key rather than dropped.
        match entry.iter().position(|&b| b == b'=') {
            Some(equals) => {
                let key = String::from_utf8_lossy(&entry[..equals]).into_owned();
                metadata
                    .entries
                    .push(MetadataEntry::new(&key, &entry[equals + 1..]));
            }
            None => {
                metadata.entries.push(MetadataEntry::new("", entry));
            }
        }
    }
    Ok(())
}

/// Parses the `fLaC` marker and metadata blocks of a FLAC stream.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short marker or block, `InvalidData`
/// for a bad marker, a missing or malformed STREAMINFO block, a
/// reserved metadata type, or a block that overruns the input, and
/// `AllocationLimit` when the metadata exceeds `MAX_METADATA_BYTES`.
pub fn parse_flac_stream(data: &[u8]) -> Result<FlacStream> {
    if data.len() < 4 {
        return Err(Error::Truncated {
            needed: 4,
            had: data.len(),
        });
    }
    if &data[0..4] != b"fLaC" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "missing fLaC marker",
        });
    }
    let mut offset = 4usize;
    let mut streaminfo: Option<StreamInfo> = None;
    let mut metadata = Metadata::new();
    let mut metadata_bytes: usize = 0;
    loop {
        let header = data.get(offset..offset + 4).ok_or(Error::Truncated {
            needed: offset + 4,
            had: data.len(),
        })?;
        let last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7F;
        let length =
            (usize::from(header[1]) << 16) | (usize::from(header[2]) << 8) | usize::from(header[3]);
        if block_type == 127 {
            return Err(Error::InvalidData {
                offset,
                detail: "reserved metadata block type 127",
            });
        }
        metadata_bytes = metadata_bytes.saturating_add(length);
        if metadata_bytes > MAX_METADATA_BYTES {
            return Err(Error::AllocationLimit {
                requested: metadata_bytes,
                limit: MAX_METADATA_BYTES,
            });
        }
        let body_start = offset + 4;
        let body = data
            .get(body_start..body_start + length)
            .ok_or(Error::Truncated {
                needed: body_start + length,
                had: data.len(),
            })?;
        match block_type {
            0 => {
                if streaminfo.is_some() {
                    return Err(Error::InvalidData {
                        offset,
                        detail: "duplicate streaminfo block",
                    });
                }
                if length != 34 {
                    return Err(Error::InvalidData {
                        offset,
                        detail: "streaminfo block must be 34 bytes",
                    });
                }
                streaminfo = Some(parse_streaminfo(body)?);
            }
            4 => parse_vorbis_comment(body, &mut metadata)?,
            // Padding, application, seek table, cuesheet, picture, and
            // unknown block types are skipped by length; their contents
            // do not affect decoding.
            _ => {}
        }
        offset = body_start + length;
        if last {
            break;
        }
    }
    let streaminfo = streaminfo.ok_or(Error::InvalidData {
        offset: 4,
        detail: "first metadata block must be streaminfo",
    })?;
    Ok(FlacStream {
        streaminfo,
        metadata,
        first_frame_offset: offset,
    })
}

/// True when `packet` is exactly one FLAC metadata block: a block
/// header whose declared length covers the rest of the packet. Used to
/// tell Ogg FLAC header packets apart from audio frames, which start
/// with the frame sync instead.
#[must_use]
pub fn is_metadata_block(packet: &[u8]) -> bool {
    if packet.len() < 4 {
        return false;
    }
    let block_type = packet[0] & 0x7F;
    if block_type == 127 {
        return false;
    }
    let length =
        (usize::from(packet[1]) << 16) | (usize::from(packet[2]) << 8) | usize::from(packet[3]);
    length + 4 == packet.len()
}

/// Parses the first packet of a FLAC-in-Ogg stream (RFC 9639 section
/// 10.1): `0x7F "FLAC"`, mapping version 1.0, a big-endian count of the
/// header packets that follow, then the `fLaC` marker and the
/// STREAMINFO block.
///
/// # Errors
///
/// Returns `Truncated` for a packet shorter than 51 bytes,
/// `InvalidData` for a bad packet prefix or streaminfo block, and
/// `Unsupported` for a mapping version other than 1.0.
pub fn parse_ogg_flac_first_packet(packet: &[u8]) -> Result<(StreamInfo, u16)> {
    if packet.len() < 51 {
        return Err(Error::Truncated {
            needed: 51,
            had: packet.len(),
        });
    }
    if packet[0] != 0x7F || &packet[1..5] != b"FLAC" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "ogg flac first packet must start with 0x7F FLAC",
        });
    }
    if packet[5] != 0x01 || packet[6] != 0x00 {
        return Err(Error::Unsupported("ogg flac mapping version"));
    }
    let header_packet_count = u16::from_be_bytes([packet[7], packet[8]]);
    if &packet[9..13] != b"fLaC" {
        return Err(Error::InvalidData {
            offset: 9,
            detail: "ogg flac first packet missing fLaC marker",
        });
    }
    if packet[13] & 0x7F != 0 {
        return Err(Error::InvalidData {
            offset: 13,
            detail: "ogg flac streaminfo block has wrong type",
        });
    }
    let length =
        (usize::from(packet[14]) << 16) | (usize::from(packet[15]) << 8) | usize::from(packet[16]);
    if length != 34 {
        return Err(Error::InvalidData {
            offset: 14,
            detail: "ogg flac streaminfo block must be 34 bytes",
        });
    }
    let streaminfo = parse_streaminfo(&packet[17..51])?;
    Ok((streaminfo, header_packet_count))
}

/// How the two channels of a stereo frame are stored (RFC 9639 section
/// 9.1.3). Independent channels need no decorrelation; the other three
/// store one channel as a difference or average.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelAssignment {
    /// 1 to 8 independent channels.
    Independent(u32),
    /// Channel 0 is left, channel 1 is left minus right.
    LeftSide,
    /// Channel 0 is left minus right, channel 1 is right.
    SideRight,
    /// Channel 0 is the mid channel, channel 1 is left minus right.
    MidSide,
}

/// A parsed frame header (RFC 9639 section 9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Interchannel samples in this frame.
    pub block_size: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels (1..=8).
    pub channels: u32,
    /// Channel assignment from the header.
    pub assignment: ChannelAssignment,
    /// Bits per sample for a non-side channel.
    pub bits_per_sample: u32,
    /// Frame or sample number from the coded number field.
    pub coded_number: u64,
    /// True for variable block size streams (sample number), false for
    /// fixed block size streams (frame number).
    pub blocking_strategy_variable: bool,
}

const SAMPLE_RATES: [u32; 12] = [
    0, 88_200, 176_400, 192_000, 8000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000,
];

fn read_coded_number(data: &[u8], offset: usize) -> Result<(u64, usize)> {
    let first = *data.get(offset).ok_or(Error::Truncated {
        needed: offset + 1,
        had: data.len(),
    })?;
    // Leading byte selects the length; 0x80..=0xBF and 0xFF are invalid.
    let (length, value_bits): (usize, u64) = match first {
        0x00..=0x7F => (1, u64::from(first & 0x7F)),
        0xC0..=0xDF => (2, u64::from(first & 0x1F)),
        0xE0..=0xEF => (3, u64::from(first & 0x0F)),
        0xF0..=0xF7 => (4, u64::from(first & 0x07)),
        0xF8..=0xFB => (5, u64::from(first & 0x03)),
        0xFC..=0xFD => (6, u64::from(first & 0x01)),
        0xFE => (7, 0),
        _ => {
            return Err(Error::InvalidData {
                offset,
                detail: "invalid coded number leading byte",
            });
        }
    };
    if data.len() < offset + length {
        return Err(Error::Truncated {
            needed: offset + length,
            had: data.len(),
        });
    }
    let mut value = value_bits;
    for i in 1..length {
        let byte = data[offset + i];
        if byte & 0xC0 != 0x80 {
            return Err(Error::InvalidData {
                offset: offset + i,
                detail: "coded number continuation byte must start with 10",
            });
        }
        value = (value << 6) | u64::from(byte & 0x3F);
    }
    // Reject overlong encodings: the value must need this many bytes.
    let minimum: u64 = match length {
        1 => 0,
        2 => 0x80,
        3 => 0x800,
        4 => 0x1_0000,
        5 => 0x20_0000,
        6 => 0x400_0000,
        _ => 0x8000_0000,
    };
    if value < minimum {
        return Err(Error::InvalidData {
            offset,
            detail: "overlong coded number",
        });
    }
    Ok((value, length))
}

fn read_u8_at(data: &[u8], offset: usize) -> Result<u8> {
    data.get(offset).copied().ok_or(Error::Truncated {
        needed: offset + 1,
        had: data.len(),
    })
}

fn read_u16_be_at(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::Truncated {
        needed: offset + 2,
        had: data.len(),
    })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Parses a frame header. Returns the header and the header length in
/// bytes, including the CRC-8 byte.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short header, `InvalidData` for a bad
/// sync, a reserved code, a CRC-8 mismatch, or a value the spec
/// forbids, and `Overflow` for a sample rate above the library limit.
///
/// Block size from the frame header code, reading the trailing
/// 8- or 16-bit size for codes 6 and 7 (RFC 9639 section 9.1.3).
/// `offset` points just past the coded frame/sample number and is
/// advanced past the trailing size when one is present.
fn resolve_block_size(data: &[u8], offset: &mut usize, code: u8) -> Result<u32> {
    match code {
        0 | 16..=u8::MAX => Err(Error::InvalidData {
            offset: 2,
            detail: "reserved block size code",
        }),
        1 => Ok(192),
        2..=5 => Ok(576u32 << (code - 2)),
        6 => {
            let value = u32::from(read_u8_at(data, *offset)?) + 1;
            *offset += 1;
            Ok(value)
        }
        7 => {
            let value = u32::from(read_u16_be_at(data, *offset)?) + 1;
            if value == 65536 {
                return Err(Error::InvalidData {
                    offset: *offset,
                    detail: "block size 65536 is forbidden",
                });
            }
            *offset += 2;
            Ok(value)
        }
        8..=15 => Ok(256u32 << (code - 8)),
    }
}

/// Sample rate from the frame header code, reading the trailing
/// 8- or 16-bit rate for codes 12-14 (RFC 9639 section 9.1.3).
/// Code 0 means the rate from STREAMINFO. `offset` is advanced past
/// the trailing rate when one is present.
fn resolve_sample_rate(
    data: &[u8],
    offset: &mut usize,
    code: u8,
    streaminfo: &StreamInfo,
) -> Result<u32> {
    let rate = match code {
        0 => streaminfo.sample_rate,
        1..=11 => SAMPLE_RATES[usize::from(code)],
        12 => {
            let value = u32::from(read_u8_at(data, *offset)?) * 1000;
            *offset += 1;
            value
        }
        13 => {
            let value = u32::from(read_u16_be_at(data, *offset)?);
            *offset += 2;
            value
        }
        14 => {
            let value = u32::from(read_u16_be_at(data, *offset)?) * 10;
            *offset += 2;
            value
        }
        _ => {
            return Err(Error::InvalidData {
                offset: 2,
                detail: "invalid sample rate code",
            });
        }
    };
    if rate == 0 {
        return Err(Error::InvalidData {
            offset: 2,
            detail: "sample rate is unknown",
        });
    }
    if rate > MAX_SAMPLE_RATE {
        return Err(Error::Overflow("frame sample rate exceeds maximum"));
    }
    Ok(rate)
}

/// Parses a frame header. Returns the header and the header length in
/// bytes, including the CRC-8 byte.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short header, `InvalidData` for a bad
/// sync, a reserved code, a CRC-8 mismatch, or a value the spec
/// forbids, and `Overflow` for a sample rate above the library limit.
pub fn parse_frame_header(data: &[u8], streaminfo: &StreamInfo) -> Result<(FrameHeader, usize)> {
    if data.len() < 4 {
        return Err(Error::Truncated {
            needed: 4,
            had: data.len(),
        });
    }
    if data[0] != 0xFF || (data[1] & 0xFE) != 0xF8 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "missing frame sync",
        });
    }
    let blocking_strategy_variable = data[1] & 0x01 != 0;
    let block_size_code = data[2] >> 4;
    let sample_rate_code = data[2] & 0x0F;
    let channel_code = data[3] >> 4;
    let bps_code = (data[3] >> 1) & 0x07;
    if data[3] & 0x01 != 0 {
        return Err(Error::InvalidData {
            offset: 3,
            detail: "frame header reserved bit must be zero",
        });
    }
    let mut offset = 4usize;
    let (coded_number, coded_len) = read_coded_number(data, offset)?;
    offset += coded_len;
    let block_size = resolve_block_size(data, &mut offset, block_size_code)?;
    let sample_rate = resolve_sample_rate(data, &mut offset, sample_rate_code, streaminfo)?;
    let (channels, assignment) = match channel_code {
        0..=7 => (
            u32::from(channel_code) + 1,
            ChannelAssignment::Independent(u32::from(channel_code) + 1),
        ),
        8 => (2, ChannelAssignment::LeftSide),
        9 => (2, ChannelAssignment::SideRight),
        10 => (2, ChannelAssignment::MidSide),
        _ => {
            return Err(Error::InvalidData {
                offset: 3,
                detail: "reserved channel assignment",
            });
        }
    };
    if channels > MAX_CHANNELS {
        return Err(Error::Overflow("frame channel count exceeds maximum"));
    }
    let bits_per_sample = match bps_code {
        0 => streaminfo.bits_per_sample,
        1 => 8,
        2 => 12,
        // Code 3 is reserved; values above 7 are impossible for a
        // 3-bit code but the compiler sees a `u8`.
        3 | 8..=u8::MAX => {
            return Err(Error::InvalidData {
                offset: 3,
                detail: "reserved bit depth code",
            });
        }
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
    };
    if !(4..=32).contains(&bits_per_sample) {
        return Err(Error::InvalidData {
            offset: 3,
            detail: "bit depth out of range",
        });
    }
    let crc_offset = offset;
    let stored_crc = read_u8_at(data, crc_offset)?;
    if crc8(&data[..crc_offset]) != stored_crc {
        return Err(Error::InvalidData {
            offset: crc_offset,
            detail: "frame header crc mismatch",
        });
    }
    Ok((
        FrameHeader {
            block_size,
            sample_rate,
            channels,
            assignment,
            bits_per_sample,
            coded_number,
            blocking_strategy_variable,
        },
        crc_offset + 1,
    ))
}

fn read_signed(reader: &mut BitReader<'_>, bits: u32) -> Result<i64> {
    if bits == 0 {
        return Ok(0);
    }
    if bits > 32 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "signed read wider than 32 bits",
        });
    }
    // The generic path handles all widths including 32: a set sign
    // bit subtracts 2^bits, which is exactly two's-complement.
    let raw = reader.read_bits(bits)?;
    let raw = i64::from(raw);
    let sign_bit = 1i64 << (bits - 1);
    if raw & sign_bit != 0 {
        Ok(raw - (1i64 << bits))
    } else {
        Ok(raw)
    }
}

fn read_rice(reader: &mut BitReader<'_>, parameter: u32) -> Result<i64> {
    // Count zero bits until the terminating one bit. The count is
    // bounded by the remaining input: `read_bit` fails with `Truncated`
    // when the bits run out.
    let mut quotient: u64 = 0;
    loop {
        if reader.read_bit()? {
            break;
        }
        quotient += 1;
        if quotient > (1u64 << 31) {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "rice quotient exceeds residual range",
            });
        }
    }
    let remainder = if parameter == 0 {
        0
    } else {
        u64::from(reader.read_bits(parameter)?)
    };
    let folded = (quotient << parameter) | remainder;
    // Zigzag unfold: even values are non-negative, odd values negative.
    // `folded / 2` is below 2^61 here (quotient is bounded above), so
    // the conversion cannot fail in practice; it is still checked.
    let half = i64::try_from(folded / 2)
        .map_err(|_| Error::Overflow("rice code value exceeds i64 range"))?;
    let residual: i64 = if folded % 2 == 0 { half } else { -half - 1 };
    // RFC 9639 section 9.2.7.3: |residual| < 2^31.
    if residual <= -(1i64 << 31) || residual >= (1i64 << 31) {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "residual sample out of range",
        });
    }
    Ok(residual)
}

fn decode_residual(
    reader: &mut BitReader<'_>,
    block_size: usize,
    predictor_order: usize,
) -> Result<Vec<i64>> {
    let method = reader.read_bits(2)?;
    let parameter_bits = match method {
        0 => 4,
        1 => 5,
        _ => {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "reserved residual coding method",
            });
        }
    };
    let partition_order = reader.read_bits(4)? as usize;
    let partitions = 1usize << partition_order;
    if block_size % partitions != 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "block size not divisible by partition count",
        });
    }
    let per_partition = block_size / partitions;
    if per_partition <= predictor_order {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "partition size not larger than predictor order",
        });
    }
    let mut residual = Vec::with_capacity(block_size - predictor_order);
    for partition in 0..partitions {
        let count = if partition == 0 {
            per_partition - predictor_order
        } else {
            per_partition
        };
        let parameter = reader.read_bits(parameter_bits)?;
        let escape = (1u32 << parameter_bits) - 1;
        if parameter == escape {
            // Escaped partition: fixed-length raw samples.
            let raw_bits = reader.read_bits(5)?;
            if raw_bits > 32 {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "escaped partition raw bit count out of range",
                });
            }
            for _ in 0..count {
                residual.push(read_signed(reader, raw_bits)?);
            }
        } else {
            for _ in 0..count {
                residual.push(read_rice(reader, parameter)?);
            }
        }
    }
    Ok(residual)
}

/// Checked `acc + coefficient * sample` for predictor
/// restoration. The RFC 9639 Appendix A.3 bounds hold only for
/// valid streams; a corrupt stream can drive the prediction past
/// i64 (found by fuzzing), so overflow is an error, not a panic.
fn checked_predict_term(acc: i64, coefficient: i64, sample: i64) -> Result<i64> {
    let term = coefficient
        .checked_mul(sample)
        .ok_or(Error::Overflow("predictor overflows i64"))?;
    acc.checked_add(term)
        .ok_or(Error::Overflow("predictor overflows i64"))
}

fn restore_fixed(samples: &mut Vec<i64>, order: usize, residual: &[i64]) -> Result<()> {
    // `samples` starts with `order` warm-up samples; the prediction
    // coefficients are the fixed predictors of RFC 9639 table 20.
    let coefficients: &[i64] = match order {
        1 => &[1],
        2 => &[2, -1],
        3 => &[3, -3, 1],
        4 => &[4, -6, 4, -1],
        // Order 0 and anything above 4 (unreachable: the caller
        // passes 0..=4) predict zero.
        _ => &[],
    };
    for (i, &value) in residual.iter().enumerate() {
        let n = order + i;
        let mut prediction: i64 = 0;
        for (j, &coefficient) in coefficients.iter().enumerate() {
            prediction = checked_predict_term(prediction, coefficient, samples[n - 1 - j])?;
        }
        samples.push(
            value
                .checked_add(prediction)
                .ok_or(Error::Overflow("fixed predictor overflows i64"))?,
        );
    }
    Ok(())
}

fn restore_lpc(
    samples: &mut Vec<i64>,
    coefficients: &[i64],
    shift: u32,
    residual: &[i64],
) -> Result<()> {
    // Coefficients are stored in past-sample order: coefficient 0
    // multiplies the sample directly before the one being predicted
    // (RFC 9639 section 9.2.6). The shift is arithmetic (all shifts in
    // the spec are) and non-negative by section 9.2.6.
    let order = coefficients.len();
    for (i, &value) in residual.iter().enumerate() {
        let n = order + i;
        let mut sum: i64 = 0;
        for (j, &coefficient) in coefficients.iter().enumerate() {
            sum = checked_predict_term(sum, coefficient, samples[n - 1 - j])?;
        }
        samples.push(
            value
                .checked_add(sum >> shift)
                .ok_or(Error::Overflow("lpc predictor overflows i64"))?,
        );
    }
    Ok(())
}

/// Wasted-bits count from the subframe header (RFC 9639 section
/// 9.2.2): unary, (wasted - 1) zero bits followed by a one bit.
/// Returns 0 when the wasted-bits flag is clear.
fn read_wasted_bits(reader: &mut BitReader<'_>) -> Result<u32> {
    if !reader.read_bit()? {
        return Ok(0);
    }
    let mut zeros: u32 = 0;
    loop {
        if reader.read_bit()? {
            break;
        }
        zeros += 1;
        if zeros >= 32 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "wasted bits count out of range",
            });
        }
    }
    Ok(zeros + 1)
}

fn decode_subframe(
    reader: &mut BitReader<'_>,
    block_size: usize,
    bits_per_sample: u32,
) -> Result<Vec<i64>> {
    if reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "subframe padding bit must be zero",
        });
    }
    let subframe_type = u8::try_from(reader.read_bits(6)?).map_err(|_| Error::InvalidData {
        offset: reader.bit_position() / 8,
        detail: "subframe type exceeds u8 range",
    })?;
    let wasted = read_wasted_bits(reader)?;
    if wasted >= bits_per_sample {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "wasted bits leave zero bits per sample",
        });
    }
    let effective_bps = bits_per_sample - wasted;
    let mut samples = match subframe_type {
        0 => {
            let value = read_signed(reader, effective_bps)?;
            vec![value; block_size]
        }
        1 => {
            let mut values = Vec::with_capacity(block_size);
            for _ in 0..block_size {
                values.push(read_signed(reader, effective_bps)?);
            }
            values
        }
        8..=12 => {
            let order = (subframe_type - 8) as usize;
            let mut values = Vec::with_capacity(block_size);
            for _ in 0..order {
                values.push(read_signed(reader, effective_bps)?);
            }
            let residual = decode_residual(reader, block_size, order)?;
            restore_fixed(&mut values, order, &residual)?;
            values
        }
        32..=63 => {
            let order = (subframe_type - 31) as usize;
            let mut values = Vec::with_capacity(block_size);
            for _ in 0..order {
                values.push(read_signed(reader, effective_bps)?);
            }
            let precision_minus_one = reader.read_bits(4)?;
            if precision_minus_one == 15 {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "lpc coefficient precision 15 is forbidden",
                });
            }
            let precision = precision_minus_one + 1;
            let shift = read_signed(reader, 5)?;
            if shift < 0 {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "lpc shift must not be negative",
                });
            }
            let mut coefficients = Vec::with_capacity(order);
            for _ in 0..order {
                coefficients.push(read_signed(reader, precision)?);
            }
            let residual = decode_residual(reader, block_size, order)?;
            // The shift is a signed 5-bit value already checked
            // non-negative, so it is 0..=15 and the conversion holds.
            let shift = u32::try_from(shift).map_err(|_| Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "lpc shift out of range",
            })?;
            restore_lpc(&mut values, &coefficients, shift, &residual)?;
            values
        }
        _ => {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "reserved subframe type",
            });
        }
    };
    if wasted > 0 {
        for sample in &mut samples {
            *sample <<= wasted;
        }
    }
    Ok(samples)
}

fn subframe_bits_per_sample(
    assignment: ChannelAssignment,
    channel_index: usize,
    bits_per_sample: u32,
) -> u32 {
    // A side subframe carries one extra bit (RFC 9639 section 9.2.3).
    let is_side = match assignment {
        ChannelAssignment::Independent(_) => false,
        // Left/side and mid/side both put the side channel second.
        ChannelAssignment::LeftSide | ChannelAssignment::MidSide => channel_index == 1,
        ChannelAssignment::SideRight => channel_index == 0,
    };
    if is_side {
        bits_per_sample + 1
    } else {
        bits_per_sample
    }
}

fn restore_channels(
    assignment: ChannelAssignment,
    subframes: Vec<Vec<i64>>,
    block_size: usize,
) -> Vec<Vec<i64>> {
    match assignment {
        ChannelAssignment::Independent(_) => subframes,
        ChannelAssignment::LeftSide => {
            let left = subframes[0].clone();
            let right: Vec<i64> = subframes[0]
                .iter()
                .zip(subframes[1].iter())
                .map(|(l, s)| l - s)
                .collect();
            debug_assert_eq!(right.len(), block_size);
            vec![left, right]
        }
        ChannelAssignment::SideRight => {
            let left: Vec<i64> = subframes[0]
                .iter()
                .zip(subframes[1].iter())
                .map(|(s, r)| s + r)
                .collect();
            debug_assert_eq!(left.len(), block_size);
            vec![left, subframes[1].clone()]
        }
        ChannelAssignment::MidSide => {
            // The mid channel stores (left + right) >> 1, dropping the
            // low bit; that bit is recovered from the side channel,
            // because left + right and left - right have the same
            // parity. This is the reconstruction used by the reference
            // decoder.
            let mut left = Vec::with_capacity(block_size);
            let mut right = Vec::with_capacity(block_size);
            for (mid, side) in subframes[0].iter().zip(subframes[1].iter()) {
                let doubled_mid = (mid << 1) | (side & 1);
                left.push((doubled_mid + side) / 2);
                right.push((doubled_mid - side) / 2);
            }
            vec![left, right]
        }
    }
}

/// Converts a decoded integer sample to `f32`. The conversion rounds
/// 32-bit samples below the f32 mantissa; that loss is inherent to
/// the f32 output format, so the lint is allowed here only.
#[allow(clippy::cast_precision_loss)]
fn sample_to_f32(value: i32) -> f32 {
    value as f32
}

/// Decodes one FLAC frame. Returns the audio frame and the number of
/// bytes the frame occupies in `data`, including header and footer.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short frame, `InvalidData` for a bad
/// sync, a reserved code, a CRC-8 or CRC-16 mismatch, a residual out
/// of range, or a decoded sample outside the declared bit depth, and
/// `Overflow` for a sample rate above the library limit.
pub fn decode_flac_frame(data: &[u8], streaminfo: &StreamInfo) -> Result<(AudioFrame, usize)> {
    let (header, header_len) = parse_frame_header(data, streaminfo)?;
    if header.block_size as usize > MAX_FRAME_SAMPLES / header.channels as usize {
        return Err(Error::AllocationLimit {
            requested: header.block_size as usize * header.channels as usize * 4,
            limit: MAX_FRAME_SAMPLES * 4,
        });
    }
    let mut reader = BitReader::new(&data[header_len..]);
    let mut subframes = Vec::with_capacity(header.channels as usize);
    for channel in 0..header.channels as usize {
        let bps = subframe_bits_per_sample(header.assignment, channel, header.bits_per_sample);
        subframes.push(decode_subframe(
            &mut reader,
            header.block_size as usize,
            bps,
        )?);
    }
    let channels = restore_channels(header.assignment, subframes, header.block_size as usize);
    // A valid encoder never produces a sample outside the declared bit
    // depth; anything else is corrupt input that happened to pass the
    // CRC, so reject it rather than returning out-of-range audio.
    let minimum = -(1i64 << (header.bits_per_sample - 1));
    let maximum = (1i64 << (header.bits_per_sample - 1)) - 1;
    for channel in &channels {
        for &sample in channel {
            if sample < minimum || sample > maximum {
                return Err(Error::InvalidData {
                    offset: header_len,
                    detail: "decoded sample out of range",
                });
            }
        }
    }
    reader.align_to_byte();
    let footer_offset = header_len + reader.bit_position() / 8;
    let footer = data
        .get(footer_offset..footer_offset + 2)
        .ok_or(Error::Truncated {
            needed: footer_offset + 2,
            had: data.len(),
        })?;
    let stored_crc = u16::from_be_bytes([footer[0], footer[1]]);
    if crc16(&data[..footer_offset]) != stored_crc {
        return Err(Error::InvalidData {
            offset: footer_offset,
            detail: "frame crc mismatch",
        });
    }
    let consumed = footer_offset + 2;
    // Samples were range-checked above to fit the declared bit depth
    // (at most 32 bits), so every value fits in an `i32`.
    let exponent = i32::try_from(header.bits_per_sample - 1)
        .map_err(|_| Error::Overflow("bit depth out of range"))?;
    let scale = 2f32.powi(exponent);
    let mut samples = Vec::with_capacity(header.block_size as usize * header.channels as usize);
    for index in 0..header.block_size as usize {
        for channel in &channels {
            let value = i32::try_from(channel[index])
                .map_err(|_| Error::Overflow("decoded sample exceeds i32 range"))?;
            samples.push(sample_to_f32(value) / scale);
        }
    }
    Ok((
        AudioFrame {
            sample_rate: header.sample_rate,
            channels: header.channels,
            samples,
            timestamp: 0,
        },
        consumed,
    ))
}

/// Demuxer for a single FLAC file. `next_packet` returns one packet
/// per frame. Frame boundaries are found by decoding each frame once
/// here; `Decoder` decodes it again, which is a known limitation.
#[derive(Debug)]
pub struct FlacDemuxer {
    bytes: Vec<u8>,
    streaminfo: StreamInfo,
    metadata: Metadata,
    position: usize,
    samples_decoded: u64,
}

impl FlacDemuxer {
    /// Reads and parses a FLAC file from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short file, `InvalidData` for a
    /// bad marker or metadata, `Unsupported` for an unknown feature,
    /// and `AllocationLimit` when the file exceeds `MAX_FLAC_FILE_BYTES`.
    pub fn new<R: Read>(reader: R) -> Result<Self> {
        let bytes = read_flac_bytes(reader)?;
        let stream = parse_flac_stream(&bytes)?;
        Ok(Self {
            bytes,
            streaminfo: stream.streaminfo,
            metadata: stream.metadata,
            position: stream.first_frame_offset,
            samples_decoded: 0,
        })
    }

    /// The parsed STREAMINFO block.
    #[must_use]
    pub fn streaminfo(&self) -> &StreamInfo {
        &self.streaminfo
    }

    /// Container metadata (Vorbis comment entries).
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Returns the next frame packet, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Propagates frame decode errors (`InvalidData` for a bad CRC or
    /// reserved code, `Truncated` for a cut-short frame).
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.position >= self.bytes.len() {
            return Ok(None);
        }
        let (frame, consumed) = decode_flac_frame(&self.bytes[self.position..], &self.streaminfo)?;
        let packet = Packet {
            codec: Codec::Flac,
            data: self.bytes[self.position..self.position + consumed].to_vec(),
            timestamp: if self.streaminfo.sample_rate == 0 {
                0
            } else {
                self.samples_decoded * 1_000_000 / u64::from(self.streaminfo.sample_rate)
            },
            is_keyframe: true,
        };
        self.samples_decoded += frame.frame_count() as u64;
        self.position += consumed;
        Ok(Some(packet))
    }
}

/// Full-frame size in interchannel samples for the encoder
/// (RFC 9639 allows up to 65535, but 4096 is the frame size this
/// encoder uses; the last frame is the remainder).
pub const FLAC_FRAME_SAMPLES: usize = 4096;

/// LPC coefficient precision in bits (RFC 9639 section 9.2.6
/// forbids precision 16, so 15 is the finest available).
const LPC_PRECISION: u32 = 15;

/// Largest magnitude a quantized LPC coefficient may have
/// (15-bit signed).
const LPC_MAX_COEFFICIENT: i64 = (1 << (LPC_PRECISION - 1)) - 1;

/// Writes `value` as a FLAC coded number (RFC 9639 section 9.1.2):
/// the UTF-8-like variable-length form, minimal length. Values at
/// or above 2^36 cannot be represented and return `Overflow`.
fn write_coded_number(value: u64, out: &mut Vec<u8>) -> Result<()> {
    // Length by magnitude; the first byte holds the remaining high
    // bits after the continuation bytes take 6 bits each.
    let length = if value < 0x80 {
        1
    } else if value < 0x800 {
        2
    } else if value < 0x1_0000 {
        3
    } else if value < 0x20_0000 {
        4
    } else if value < 0x400_0000 {
        5
    } else if value < 0x8000_0000 {
        6
    } else if value < (1u64 << 36) {
        7
    } else {
        return Err(Error::Overflow("flac coded number exceeds 36 bits"));
    };
    let first_prefix: u8 = match length {
        1 => 0x00,
        2 => 0xC0,
        3 => 0xE0,
        4 => 0xF0,
        5 => 0xF8,
        6 => 0xFC,
        _ => 0xFE,
    };
    let first_bits = if length == 1 { 7 } else { 8 - length };
    let high = if first_bits == 0 {
        0
    } else {
        u8::try_from((value >> (6 * (length - 1))) & ((1u64 << first_bits) - 1)).unwrap_or(0)
    };
    out.push(first_prefix | high);
    for i in (0..length - 1).rev() {
        let group = u8::try_from((value >> (6 * i)) & 0x3F).unwrap_or(0);
        out.push(0x80 | group);
    }
    Ok(())
}

/// Writes the 34-byte STREAMINFO block (RFC 9639 section 8.2).
/// This is the inverse of `parse_streaminfo`. The caller must
/// supply a valid block: channels 1..=8, bits per sample 4..=32,
/// total samples fitting in 36 bits (larger totals are masked to
/// the field width, matching the on-wire format).
#[must_use]
pub fn write_streaminfo_bytes(streaminfo: &StreamInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(34);
    out.extend_from_slice(&streaminfo.min_block_size.to_be_bytes());
    out.extend_from_slice(&streaminfo.max_block_size.to_be_bytes());
    out.extend_from_slice(&streaminfo.min_frame_size.to_be_bytes()[1..]);
    out.extend_from_slice(&streaminfo.max_frame_size.to_be_bytes()[1..]);
    // Bytes 10-17 pack sample rate (20 bits), channels minus 1
    // (3 bits), bits per sample minus 1 (5 bits), and total samples
    // (36 bits) into 64 bits, big-endian.
    let packed: u64 = (u64::from(streaminfo.sample_rate) << 44)
        | (u64::from(streaminfo.channels.saturating_sub(1)) << 41)
        | (u64::from(streaminfo.bits_per_sample.saturating_sub(1)) << 36)
        | (streaminfo.total_samples & ((1u64 << 36) - 1));
    out.extend_from_slice(&packed.to_be_bytes());
    out.extend_from_slice(&streaminfo.md5);
    out
}

/// Writes the `fLaC` marker plus the single STREAMINFO metadata
/// block (last-block flag set, type 0, length 34).
#[must_use]
pub fn write_flac_stream_header(streaminfo: &StreamInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(42);
    out.extend_from_slice(b"fLaC");
    out.push(0x80);
    out.push(0);
    out.push(0);
    out.push(34);
    out.extend_from_slice(&write_streaminfo_bytes(streaminfo));
    out
}

/// Zigzag fold for Rice coding (RFC 9639 section 9.2.7.2):
/// 0 -> 0, -1 -> 1, 1 -> 2, -2 -> 3, 2 -> 4, ...
#[allow(clippy::cast_sign_loss)]
fn zigzag_fold(value: i64) -> u64 {
    // The fold intentionally reinterprets the two's-complement bit
    // pattern: a non-negative value doubles, a negative value maps
    // to `2 * |value| - 1`.
    if value >= 0 {
        (value as u64) << 1
    } else {
        (((-(value + 1)) as u64) << 1) | 1
    }
}

/// Exact bit cost of Rice-coding `residual` with `parameter` in a
/// single partition: per value, the unary quotient bits, the
/// terminating one bit, and the `parameter` remainder bits.
fn rice_cost(residual: &[i64], parameter: u32) -> u64 {
    let mut cost: u64 = 0;
    for &value in residual {
        let folded = zigzag_fold(value);
        cost += (folded >> parameter) + 1 + u64::from(parameter);
    }
    cost
}

/// Best Rice parameter (0..=14) for `residual` and its exact bit
/// cost. Parameter 15 is the escape code and is never emitted: a
/// residual that costs more than verbatim is written verbatim by
/// the caller instead.
fn best_rice_parameter(residual: &[i64]) -> (u32, u64) {
    let mut best_parameter = 0u32;
    let mut best_cost = u64::MAX;
    for parameter in 0..=14u32 {
        let cost = rice_cost(residual, parameter);
        if cost < best_cost {
            best_cost = cost;
            best_parameter = parameter;
        }
    }
    (best_parameter, best_cost)
}

/// Writes one Rice-coded residual: quotient in unary (zeros then a
/// one bit), remainder in `parameter` bits.
fn write_rice(writer: &mut BitWriter, value: i64, parameter: u32) -> Result<()> {
    let folded = zigzag_fold(value);
    let quotient = folded >> parameter;
    for _ in 0..quotient {
        writer.write_bit(false);
    }
    writer.write_bit(true);
    if parameter > 0 {
        let remainder = u32::try_from(folded & ((1u64 << parameter) - 1)).unwrap_or(0);
        writer.write_bits(remainder, parameter)?;
    }
    Ok(())
}

/// Fixed-predictor prediction (RFC 9639 table 20): the predicted
/// value of `samples[n]` from the `order` preceding samples.
/// `n >= order` is required by the caller.
fn fixed_prediction(samples: &[i64], order: usize, n: usize) -> i64 {
    match order {
        1 => samples[n - 1],
        2 => 2 * samples[n - 1] - samples[n - 2],
        3 => 3 * samples[n - 1] - 3 * samples[n - 2] + samples[n - 3],
        4 => 4 * samples[n - 1] - 6 * samples[n - 2] + 4 * samples[n - 3] - samples[n - 4],
        // Order 0 (and anything above 4, unreachable) predicts zero.
        _ => 0,
    }
}

/// Residual of a fixed-predictor subframe: `samples[n] -
/// prediction` for `n >= order`.
fn fixed_residual(samples: &[i64], order: usize) -> Vec<i64> {
    let mut residual = Vec::with_capacity(samples.len().saturating_sub(order));
    for n in order..samples.len() {
        residual.push(samples[n] - fixed_prediction(samples, order, n));
    }
    residual
}

/// Autocorrelation `r[k] = sum x[n] * x[n-k]` for `k = 0..=order`
/// (f64: i16 inputs convert exactly, and the sums stay far below
/// the 53-bit mantissa for a 4096-sample frame).
fn autocorrelation(samples: &[i16], order: usize) -> Vec<f64> {
    let mut r = vec![0.0f64; order + 1];
    for k in 0..=order {
        let mut sum = 0.0f64;
        for n in k..samples.len() {
            sum += f64::from(samples[n]) * f64::from(samples[n - k]);
        }
        r[k] = sum;
    }
    r
}

/// Levinson-Durbin solution of the Yule-Walker equations for the
/// LPC coefficients `a[1..=order]` predicting `x[n]` as
/// `sum a[j] * x[n-j]`. Verified against the closed form for a sine
/// wave (order 2 gives `2cos(w)`, `-1`). Returns all zeros for a
/// silent (zero-energy) input; stops early when the prediction
/// error reaches zero (perfect prediction).
fn levinson_durbin(r: &[f64], order: usize) -> Vec<f64> {
    let mut a = vec![0.0f64; order + 1];
    let mut error = r[0];
    if error == 0.0 {
        return a;
    }
    for i in 1..=order {
        let mut numerator = r[i];
        for j in 1..i {
            numerator -= a[j] * r[i - j];
        }
        let k = numerator / error;
        let old = a.clone();
        for j in 1..i {
            a[j] = old[j] - k * old[i - j];
        }
        a[i] = k;
        error *= 1.0 - k * k;
        if error <= 0.0 {
            break;
        }
    }
    a
}

/// Quantizes float LPC coefficients to `LPC_PRECISION`-bit signed
/// integers with a shift chosen so the largest coefficient uses the
/// available range. Returns `(coefficients, shift)`, where the
/// decoder reconstructs the prediction as
/// `(sum coef[j] * x[n-1-j]) >> shift`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn quantize_lpc(coefficients: &[f64]) -> (Vec<i64>, u32) {
    let max_magnitude = coefficients
        .iter()
        .fold(0.0f64, |max, &value| max.max(value.abs()));
    if max_magnitude == 0.0 {
        return (vec![0; coefficients.len()], 0);
    }
    // Largest shift that keeps the biggest coefficient in range;
    // a larger shift uses more of the 15 bits and so quantizes
    // more finely. Clamped to the 5-bit signed shift field (0..=15).
    // 16383 fits exactly in f64, and the rounded value is clamped
    // to the 15-bit range before the conversion, so neither cast
    // loses a value that matters.
    let mut shift: u32 = 0;
    while shift < 15 && max_magnitude * f64::from(1u32 << (shift + 1)) <= LPC_MAX_COEFFICIENT as f64
    {
        shift += 1;
    }
    let scale = f64::from(1u32 << shift);
    let quantized: Vec<i64> = coefficients
        .iter()
        .map(|&value| {
            let rounded = (value * scale).round() as i64;
            rounded.clamp(-LPC_MAX_COEFFICIENT - 1, LPC_MAX_COEFFICIENT)
        })
        .collect();
    (quantized, shift)
}

/// Residual of an LPC subframe, computed with the *quantized*
/// coefficients and shift exactly as the decoder restores them
/// (`(sum coef[j] * x[n-1-j]) >> shift`, arithmetic shift on i64),
/// so the round-trip is exact by construction.
fn lpc_residual(samples: &[i64], coefficients: &[i64], shift: u32) -> Vec<i64> {
    let order = coefficients.len();
    let mut residual = Vec::with_capacity(samples.len().saturating_sub(order));
    for n in order..samples.len() {
        let mut sum: i64 = 0;
        for (j, &coefficient) in coefficients.iter().enumerate() {
            sum += coefficient * samples[n - 1 - j];
        }
        residual.push(samples[n] - (sum >> shift));
    }
    residual
}

/// A subframe choice for one channel, with everything needed to
/// write it: the warm-up samples, the quantized predictor, and the
/// residual with its best Rice parameter.
#[derive(Debug, Clone)]
enum SubframeChoice {
    Constant(i16),
    Verbatim,
    Fixed {
        order: usize,
        residual: Vec<i64>,
        rice_parameter: u32,
    },
    Lpc {
        order: usize,
        coefficients: Vec<i64>,
        shift: u32,
        residual: Vec<i64>,
        rice_parameter: u32,
    },
}

/// Exact bit cost of a subframe choice, including the 8-bit
/// subframe header (padding bit, 6-bit type, wasted-bits flag).
fn subframe_cost(choice: &SubframeChoice, block_size: usize) -> u64 {
    const HEADER_BITS: u64 = 8;
    match choice {
        SubframeChoice::Constant(_) => HEADER_BITS + 16,
        SubframeChoice::Verbatim => HEADER_BITS + (block_size as u64) * 16,
        SubframeChoice::Fixed {
            order,
            residual,
            rice_parameter,
        } => {
            let rice_bits = rice_cost(residual, *rice_parameter);
            HEADER_BITS + (*order as u64) * 16 + 10 + rice_bits
        }
        SubframeChoice::Lpc {
            order,
            residual,
            rice_parameter,
            ..
        } => {
            let rice_bits = rice_cost(residual, *rice_parameter);
            HEADER_BITS
                + (*order as u64) * 16
                + 4
                + 5
                + (*order as u64) * u64::from(LPC_PRECISION)
                + 10
                + rice_bits
        }
    }
}

/// Chooses the cheapest subframe for one channel by exact encoded
/// size: constant, verbatim, fixed predictor orders 0–4, LPC
/// orders 1–8. A predicted subframe whose best Rice cost is not
/// better than verbatim is written verbatim. Channels are
/// independent (no mid/side decorrelation in Phase 9).
fn choose_subframe(samples: &[i16]) -> SubframeChoice {
    let block_size = samples.len();
    let verbatim = SubframeChoice::Verbatim;
    let verbatim_cost = subframe_cost(&verbatim, block_size);
    let mut best = verbatim;
    let mut best_cost = verbatim_cost;

    if let Some(&first) = samples.first() {
        if samples.iter().all(|&value| value == first) {
            let constant = SubframeChoice::Constant(first);
            let cost = subframe_cost(&constant, block_size);
            if cost < best_cost {
                best = constant;
                best_cost = cost;
            }
        }
    }

    let samples_i64: Vec<i64> = samples.iter().map(|&value| i64::from(value)).collect();
    for order in 0..=4usize {
        if block_size <= order {
            continue;
        }
        let residual = fixed_residual(&samples_i64, order);
        if residual
            .iter()
            .any(|&value| value <= -(1i64 << 31) || value >= (1i64 << 31))
        {
            continue;
        }
        let (rice_parameter, _) = best_rice_parameter(&residual);
        let choice = SubframeChoice::Fixed {
            order,
            residual,
            rice_parameter,
        };
        let cost = subframe_cost(&choice, block_size);
        if cost < best_cost {
            best_cost = cost;
            best = choice;
        }
    }

    for order in 1..=8usize {
        if block_size <= order {
            continue;
        }
        let r = autocorrelation(samples, order);
        let coefficients = levinson_durbin(&r, order);
        let (quantized, shift) = quantize_lpc(&coefficients[1..]);
        let residual = lpc_residual(&samples_i64, &quantized, shift);
        if residual
            .iter()
            .any(|&value| value <= -(1i64 << 31) || value >= (1i64 << 31))
        {
            continue;
        }
        let (rice_parameter, _) = best_rice_parameter(&residual);
        let choice = SubframeChoice::Lpc {
            order,
            coefficients: quantized,
            shift,
            residual,
            rice_parameter,
        };
        let cost = subframe_cost(&choice, block_size);
        if cost < best_cost {
            best_cost = cost;
            best = choice;
        }
    }

    best
}

/// Writes one channel's subframe (RFC 9639 section 9.2): the
/// padding bit (0), the 6-bit type, the wasted-bits flag (0), then
/// the type-specific data and the single-partition Rice residual.
fn write_subframe(writer: &mut BitWriter, samples: &[i16], choice: &SubframeChoice) -> Result<()> {
    writer.write_bits(0, 1)?;
    match choice {
        SubframeChoice::Constant(value) => {
            writer.write_bits(0, 6)?;
            writer.write_bits(0, 1)?;
            writer.write_signed(i64::from(*value), 16)?;
        }
        SubframeChoice::Verbatim => {
            writer.write_bits(1, 6)?;
            writer.write_bits(0, 1)?;
            for &sample in samples {
                writer.write_signed(i64::from(sample), 16)?;
            }
        }
        SubframeChoice::Fixed {
            order,
            residual,
            rice_parameter,
        } => {
            writer.write_bits(8 + u32::try_from(*order).unwrap_or(0), 6)?;
            writer.write_bits(0, 1)?;
            for &sample in &samples[..*order] {
                writer.write_signed(i64::from(sample), 16)?;
            }
            write_residual(writer, residual, *rice_parameter)?;
        }
        SubframeChoice::Lpc {
            order,
            coefficients,
            shift,
            residual,
            rice_parameter,
        } => {
            writer.write_bits(31 + u32::try_from(*order).unwrap_or(0), 6)?;
            writer.write_bits(0, 1)?;
            for &sample in &samples[..*order] {
                writer.write_signed(i64::from(sample), 16)?;
            }
            writer.write_bits(LPC_PRECISION - 1, 4)?;
            writer.write_signed(i64::from(*shift), 5)?;
            for &coefficient in coefficients {
                writer.write_signed(coefficient, LPC_PRECISION)?;
            }
            write_residual(writer, residual, *rice_parameter)?;
        }
    }
    Ok(())
}

/// Writes a residual as a single Rice partition (RFC 9639 section
/// 9.2.7): method 0 (4-bit Rice parameters), partition order 0,
/// then the parameter and one Rice code per residual value.
fn write_residual(writer: &mut BitWriter, residual: &[i64], rice_parameter: u32) -> Result<()> {
    writer.write_bits(0, 2)?;
    writer.write_bits(0, 4)?;
    writer.write_bits(rice_parameter, 4)?;
    for &value in residual {
        write_rice(writer, value, rice_parameter)?;
    }
    Ok(())
}

/// Frame header block-size code for `block_size` (1..=4096),
/// with the trailing size bytes appended to `out` for codes 6/7.
fn block_size_code(block_size: u32, out: &mut Vec<u8>) -> u8 {
    match block_size {
        192 => 1,
        576 => 2,
        1152 => 3,
        2304 => 4,
        4096 => 12,
        256 => 8,
        512 => 9,
        1024 => 10,
        2048 => 11,
        _ => {
            if block_size <= 256 {
                out.push(u8::try_from(block_size - 1).unwrap_or(0));
                6
            } else {
                out.extend_from_slice(&u16::try_from(block_size - 1).unwrap_or(0).to_be_bytes());
                7
            }
        }
    }
}

/// Frame header sample-rate code for `sample_rate`, with the
/// trailing rate bytes appended to `out` for codes 12–14. Falls
/// back to code 0 (rate from STREAMINFO) when no code represents
/// the rate exactly; the encoder always writes STREAMINFO, so
/// code 0 is always valid.
fn sample_rate_code(sample_rate: u32, out: &mut Vec<u8>) -> u8 {
    match sample_rate {
        88_200 => 1,
        176_400 => 2,
        192_000 => 3,
        8000 => 4,
        16_000 => 5,
        22_050 => 6,
        24_000 => 7,
        32_000 => 8,
        44_100 => 9,
        48_000 => 10,
        96_000 => 11,
        _ => {
            if sample_rate <= 65_535 {
                out.extend_from_slice(&u16::try_from(sample_rate).unwrap_or(0).to_be_bytes());
                13
            } else if sample_rate % 1000 == 0 && sample_rate / 1000 <= 255 {
                out.push(u8::try_from(sample_rate / 1000).unwrap_or(0));
                12
            } else if sample_rate % 10 == 0 {
                out.extend_from_slice(&u16::try_from(sample_rate / 10).unwrap_or(0).to_be_bytes());
                14
            } else {
                0
            }
        }
    }
}

/// Encodes one FLAC frame (RFC 9639 section 9): a frame header
/// with CRC-8, one subframe per channel, and a CRC-16 footer.
/// `samples` are interleaved i16, `block_size` =
/// `samples.len() / channels` interchannel samples (1..=4096).
/// `sample_number` is the sample number of the first sample in
/// this frame (variable blocking strategy).
///
/// # Errors
///
/// Returns `InvalidArgument` for a channel count outside 1..=8,
/// a zero or over-limit sample rate, a sample count that is not a
/// whole number of frames, or a block size outside 1..=4096, and
/// `Overflow` for a sample number the coded-number form cannot
/// represent.
pub fn encode_flac_frame(
    samples: &[i16],
    channels: u32,
    sample_rate: u32,
    sample_number: u64,
) -> Result<Vec<u8>> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "flac channel count must be between 1 and 8",
        ));
    }
    if sample_rate == 0 || sample_rate > MAX_SAMPLE_RATE {
        return Err(Error::InvalidArgument("flac sample rate out of range"));
    }
    let channels = channels as usize;
    if samples.is_empty() || samples.len() % channels != 0 {
        return Err(Error::InvalidArgument(
            "flac sample count is not a whole number of frames",
        ));
    }
    let block_size = samples.len() / channels;
    if block_size > FLAC_FRAME_SAMPLES {
        return Err(Error::InvalidArgument(
            "flac block size exceeds 4096 samples",
        ));
    }
    // Deinterleave into per-channel samples.
    let mut per_channel: Vec<Vec<i16>> = vec![Vec::with_capacity(block_size); channels];
    for (index, &sample) in samples.iter().enumerate() {
        per_channel[index % channels].push(sample);
    }

    let mut header: Vec<u8> = Vec::with_capacity(16);
    header.push(0xFF);
    header.push(0xF9);
    let mut trailing: Vec<u8> = Vec::new();
    let block_code = block_size_code(u32::try_from(block_size).unwrap_or(0), &mut trailing);
    let rate_code = sample_rate_code(sample_rate, &mut trailing);
    header.push((block_code << 4) | rate_code);
    header.push((u8::try_from(channels - 1).unwrap_or(0) << 4) | (4 << 1));
    write_coded_number(sample_number, &mut header)?;
    header.extend_from_slice(&trailing);
    header.push(crc8(&header));

    let mut writer = BitWriter::new();
    for channel_samples in &per_channel {
        let choice = choose_subframe(channel_samples);
        write_subframe(&mut writer, channel_samples, &choice)?;
    }
    let mut frame = header;
    frame.extend_from_slice(&writer.finish());
    let crc = crc16(&frame);
    frame.extend_from_slice(&crc.to_be_bytes());
    Ok(frame)
}

/// Streaming FLAC encoder: one frame per `encode` call. The
/// caller writes `streaminfo_bytes()` first, then the frames.
/// Sample numbers accumulate across calls.
#[derive(Debug)]
pub struct FlacEncoder {
    channels: u32,
    sample_rate: u32,
    sample_number: u64,
}

impl FlacEncoder {
    /// Creates a streaming encoder.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a channel count outside 1..=8
    /// or a zero or over-limit sample rate.
    pub fn new(channels: u32, sample_rate: u32) -> Result<Self> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(Error::InvalidArgument(
                "flac channel count must be between 1 and 8",
            ));
        }
        if sample_rate == 0 || sample_rate > MAX_SAMPLE_RATE {
            return Err(Error::InvalidArgument("flac sample rate out of range"));
        }
        Ok(Self {
            channels,
            sample_rate,
            sample_number: 0,
        })
    }

    /// The `fLaC` marker plus STREAMINFO in streaming form: total
    /// samples 0, minimum block size 0, maximum block size 4096,
    /// frame sizes unknown, MD5 unset.
    #[must_use]
    pub fn streaminfo_bytes(&self) -> Vec<u8> {
        write_flac_stream_header(&StreamInfo {
            min_block_size: 0,
            max_block_size: u16::try_from(FLAC_FRAME_SAMPLES).unwrap_or(0),
            min_frame_size: 0,
            max_frame_size: 0,
            sample_rate: self.sample_rate,
            channels: self.channels,
            bits_per_sample: 16,
            total_samples: 0,
            md5: [0; 16],
        })
    }

    /// Encodes one frame from interleaved i16 samples. The block
    /// size is `samples.len() / channels` (1..=4096).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for an empty input, a sample count
    /// that is not a whole number of frames, or a block size above
    /// 4096.
    pub fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        let frame =
            encode_flac_frame(samples, self.channels, self.sample_rate, self.sample_number)?;
        self.sample_number += (samples.len() / self.channels as usize) as u64;
        Ok(frame)
    }

    /// Sample number of the next frame's first sample.
    #[must_use]
    pub fn sample_number(&self) -> u64 {
        self.sample_number
    }
}

/// Encodes interleaved i16 samples as a complete FLAC file:
/// `fLaC` marker, STREAMINFO (exact min/max block size and total
/// samples, MD5 unset), then 4096-sample frames with the last
/// frame holding the remainder. Decoding the result with the
/// Phase 2 decoder returns the original samples byte-identically.
///
/// # Errors
///
/// Returns `InvalidArgument` for an empty input, a channel count
/// outside 1..=8, a zero or over-limit sample rate, or a sample
/// count that is not a whole number of frames.
pub fn encode_flac_stream(samples: &[i16], channels: u32, sample_rate: u32) -> Result<Vec<u8>> {
    if samples.is_empty() {
        return Err(Error::InvalidArgument("flac encode requires samples"));
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "flac channel count must be between 1 and 8",
        ));
    }
    let channels_usize = channels as usize;
    if samples.len() % channels_usize != 0 {
        return Err(Error::InvalidArgument(
            "flac sample count is not a whole number of frames",
        ));
    }
    let total_samples = (samples.len() / channels_usize) as u64;
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut sample_number = 0u64;
    let mut min_block_size = u16::MAX;
    let mut max_block_size = 0u16;
    for chunk in samples.chunks(FLAC_FRAME_SAMPLES * channels_usize) {
        let block_size = chunk.len() / channels_usize;
        let block_size = u16::try_from(block_size).unwrap_or(0);
        min_block_size = min_block_size.min(block_size);
        max_block_size = max_block_size.max(block_size);
        frames.push(encode_flac_frame(
            chunk,
            channels,
            sample_rate,
            sample_number,
        )?);
        sample_number += u64::from(block_size);
    }
    let min_frame_size = u32::try_from(frames.iter().map(Vec::len).min().unwrap_or(0)).unwrap_or(0);
    let max_frame_size = u32::try_from(frames.iter().map(Vec::len).max().unwrap_or(0)).unwrap_or(0);
    let mut out = write_flac_stream_header(&StreamInfo {
        min_block_size,
        max_block_size,
        min_frame_size,
        max_frame_size,
        sample_rate,
        channels,
        bits_per_sample: 16,
        total_samples,
        md5: [0; 16],
    });
    for frame in frames {
        out.extend_from_slice(&frame);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_streaminfo() -> StreamInfo {
        StreamInfo {
            min_block_size: 4608,
            max_block_size: 4608,
            min_frame_size: 0,
            max_frame_size: 0,
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 4410,
            md5: [0u8; 16],
        }
    }

    #[test]
    fn crc8_matches_known_check_value() {
        assert_eq!(crc8(b"123456789"), 0xF4);
    }

    #[test]
    fn crc16_matches_known_check_value() {
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }

    #[test]
    fn parses_streaminfo_fields() {
        let bytes = include_bytes!("../tests/data/sine_stereo_44100.flac");
        let stream = parse_flac_stream(bytes).unwrap();
        assert_eq!(stream.streaminfo.sample_rate, 44100);
        assert_eq!(stream.streaminfo.channels, 2);
        assert_eq!(stream.streaminfo.bits_per_sample, 16);
        assert_eq!(stream.streaminfo.total_samples, 4410);
        assert_eq!(stream.streaminfo.min_block_size, 4608);
        assert_eq!(stream.streaminfo.max_block_size, 4608);
    }

    #[test]
    fn vorbis_comment_metadata_is_parsed() {
        let bytes = include_bytes!("../tests/data/sine_stereo_44100.flac");
        let stream = parse_flac_stream(bytes).unwrap();
        let vendor = stream.metadata.get("VENDOR").unwrap();
        assert!(vendor.as_str().unwrap_or("").starts_with("Lavf"));
        // ffmpeg writes the key in lowercase; keys are kept as written.
        let encoder = stream.metadata.get("encoder").unwrap();
        assert_eq!(encoder.as_str().unwrap(), "Lavf62.12.102");
    }

    #[test]
    fn bad_marker_returns_invalid_data() {
        let err = parse_flac_stream(b"XXXX....").unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn short_stream_returns_truncated() {
        let err = parse_flac_stream(b"fL").unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn coded_number_decodes_extended_forms() {
        // Two-byte form: 0x80 -> 0xC2 0x80.
        let (value, len) = read_coded_number(&[0xC2, 0x80], 0).unwrap();
        assert_eq!((value, len), (0x80, 2));
        // One-byte form.
        assert_eq!(read_coded_number(&[0x05], 0).unwrap(), (5, 1));
        // Overlong two-byte form for a value below 0x80 is rejected.
        let err = read_coded_number(&[0xC0, 0x80], 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
        // A continuation byte without the 10 prefix is rejected.
        let err = read_coded_number(&[0xC2, 0x40], 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn decodes_sine_frame_header() {
        let bytes = include_bytes!("../tests/data/sine_stereo_44100.flac");
        let stream = parse_flac_stream(bytes).unwrap();
        let (header, len) =
            parse_frame_header(&bytes[stream.first_frame_offset..], &stream.streaminfo).unwrap();
        assert_eq!(header.block_size, 4410);
        assert_eq!(header.sample_rate, 44100);
        assert_eq!(header.channels, 2);
        assert_eq!(header.assignment, ChannelAssignment::LeftSide);
        assert_eq!(header.bits_per_sample, 16);
        assert!(len > 4);
    }

    #[test]
    fn corrupt_header_crc_returns_invalid_data() {
        let bytes = include_bytes!("../tests/data/sine_stereo_44100.flac");
        let stream = parse_flac_stream(bytes).unwrap();
        let mut frame = bytes[stream.first_frame_offset..].to_vec();
        // Flip a bit in the block-size code byte; the CRC-8 must catch it.
        frame[2] ^= 0x10;
        let err = parse_frame_header(&frame, &stream.streaminfo).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn frame_without_sync_returns_invalid_data() {
        let err =
            parse_frame_header(&[0x00, 0x00, 0x00, 0x00, 0x00], &sine_streaminfo()).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn ogg_flac_first_packet_parses() {
        let packet: Vec<u8> = {
            let mut packet = vec![0x7F];
            packet.extend_from_slice(b"FLAC");
            packet.extend_from_slice(&[0x01, 0x00]);
            packet.extend_from_slice(&1u16.to_be_bytes());
            packet.extend_from_slice(b"fLaC");
            packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x22]);
            let bytes = include_bytes!("../tests/data/sine_stereo_44100.flac");
            let stream = parse_flac_stream(bytes).unwrap();
            let _ = stream;
            // STREAMINFO is the first 34 bytes after the fLaC marker.
            packet.extend_from_slice(&bytes[8..42]);
            packet
        };
        let (streaminfo, count) = parse_ogg_flac_first_packet(&packet).unwrap();
        assert_eq!(streaminfo.sample_rate, 44100);
        assert_eq!(count, 1);
    }

    #[test]
    fn metadata_block_detection() {
        // A 4-byte header declaring a 2-byte body is a metadata block.
        assert!(is_metadata_block(&[0x84, 0x00, 0x00, 0x02, 0x00, 0x00]));
        // A frame sync is not a metadata block.
        assert!(!is_metadata_block(&[0xFF, 0xF8, 0x00, 0x00]));
    }
}
